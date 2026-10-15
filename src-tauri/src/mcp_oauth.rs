//! Anonymous, reviewed OAuth discovery. No browser, client registration, token
//! exchange or credential read is permitted by this module. Remote metadata is
//! descriptive; it cannot choose another issuer or expand the reviewed GET scope.
use crate::mcp_profile;
use reqwest::{header, StatusCode, Url};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

const LIMIT: usize = 65536;
#[derive(Clone, Copy)]
enum Network {
    Production,
    #[cfg(test)]
    OwnedLoopback,
}
pub(crate) struct Plan {
    resource: Url,
    issuer: String,
    metadata_url: Option<Url>,
    resource_candidates: Vec<Url>,
    server_candidates: Vec<Url>,
    network: Network,
}
fn url(text: &str, allow_loopback: bool) -> Result<Url, String> {
    url_fields(text, allow_loopback, false)
}
pub(crate) fn issuer_url(text: &str) -> Result<Url, String> {
    url(text, false)
}
fn url_fields(text: &str, allow_loopback: bool, query_allowed: bool) -> Result<Url, String> {
    if text.is_empty()
        || text.len() > 4096
        || text.chars().any(|c| c.is_whitespace() || c.is_control())
        || text.contains('\\')
        || crate::policy::contains_secret(text)
    {
        return Err("MCP_OAUTH_URL".into());
    }
    let u = Url::parse(text).map_err(|_| "MCP_OAUTH_URL")?;
    if u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || (!query_allowed && u.query().is_some())
        || u.fragment().is_some()
        || !(u.scheme() == "https"
            || (allow_loopback
                && u.scheme() == "http"
                && matches!(u.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))))
    {
        return Err("MCP_OAUTH_URL_TLS_OR_FIELDS".into());
    }
    Ok(u)
}
fn as_url(text: &str, network: Network) -> Result<Url, String> {
    let local = match network {
        Network::Production => false,
        #[cfg(test)]
        Network::OwnedLoopback => true,
    };
    url(text, local)
}
fn as_endpoint_url(text: &str, network: Network) -> Result<Url, String> {
    let local = match network {
        Network::Production => false,
        #[cfg(test)]
        Network::OwnedLoopback => true,
    };
    url_fields(text, local, true)
}
fn at(origin: &Url, path: &str) -> Url {
    let mut u = origin.clone();
    u.set_path(path);
    u
}
fn unique(urls: Vec<Url>) -> Vec<Url> {
    let mut seen = BTreeSet::new();
    urls.into_iter()
        .filter(|u| seen.insert(u.clone()))
        .collect()
}
impl Plan {
    pub(crate) fn new(c: &mcp_profile::Config, input: &Value) -> Result<Self, String> {
        Self::build(c, input, Network::Production)
    }
    fn build(c: &mcp_profile::Config, input: &Value, network: Network) -> Result<Self, String> {
        if c.transport != mcp_profile::Transport::StreamableHttp {
            return Err("MCP_OAUTH_HTTP_REQUIRED".into());
        }
        let resource = mcp_profile::endpoint(c)?;
        let issuer = input["issuer"]
            .as_str()
            .ok_or("MCP_OAUTH_ISSUER_REQUIRED")?;
        if let mcp_profile::Auth::OAuth2 { issuer: saved } = &c.auth {
            if saved != issuer {
                return Err("MCP_OAUTH_ISSUER_BINDING".into());
            }
        }
        let server = as_url(issuer, network)?;
        let metadata_url = input
            .get("metadata_url")
            .map(|v| url(v.as_str().ok_or("MCP_OAUTH_METADATA_URL")?, true))
            .transpose()?;
        if metadata_url
            .as_ref()
            .is_some_and(|u| u.origin() != resource.origin() && u.scheme() != "https")
        {
            return Err("MCP_OAUTH_METADATA_TLS".into());
        }
        let resource_candidates = unique(vec![
            at(
                &resource,
                &format!("/.well-known/oauth-protected-resource{}", resource.path()),
            ),
            at(&resource, "/.well-known/oauth-protected-resource"),
        ]);
        let path = if server.path() == "/" {
            ""
        } else {
            server.path()
        };
        let mut server_candidates = vec![
            at(
                &server,
                &format!("/.well-known/oauth-authorization-server{path}"),
            ),
            at(&server, &format!("/.well-known/openid-configuration{path}")),
        ];
        if !path.is_empty() {
            server_candidates.push(at(
                &server,
                &format!(
                    "{}/.well-known/openid-configuration",
                    path.trim_end_matches('/')
                ),
            ));
        }
        Ok(Self {
            resource,
            issuer: issuer.into(),
            metadata_url,
            resource_candidates,
            server_candidates: unique(server_candidates),
            network,
        })
    }
    pub(crate) fn review(&self) -> Value {
        json!({"resource":self.resource.as_str(),"issuer":self.issuer,
            "explicit_metadata_url":self.metadata_url.as_ref().map(Url::as_str),
            "protected_resource_candidates":self.resource_candidates.iter().map(Url::as_str).collect::<Vec<_>>(),
            "server_metadata_candidates":self.server_candidates.iter().map(Url::as_str).collect::<Vec<_>>(),
            "methods":["GET"],"anonymous":true,"redirects":false,"automatic_replay":false,
            "challenge_metadata_policy":"same resource origin, or exact explicitly reviewed metadata URL",
            "browser_launch":false,"client_registration":false,"credential_exchange":false})
    }
    pub(crate) async fn discover(&self) -> Result<Value, String> {
        let client = crate::http_client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| "MCP_OAUTH_CLIENT")?;
        // GET only; no configured private/public profile headers are inherited.
        let response = client
            .get(self.resource.clone())
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| "MCP_OAUTH_RESOURCE_NETWORK")?;
        let status = response.status();
        if !matches!(status.as_u16(), 200 | 204 | 401 | 403 | 405) {
            return Err(format!("MCP_OAUTH_RESOURCE_HTTP_{}", status.as_u16()));
        }
        let challenge = if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            challenge(response.headers())?
        } else {
            Challenge::default()
        };
        drop(response); // Do not read or adopt an SSE/business response body.
        let prm_urls = if let Some(text) = &challenge.metadata {
            let advertised = url(text, true)?;
            if let Some(explicit) = &self.metadata_url {
                if advertised != *explicit {
                    return Err("MCP_OAUTH_CHALLENGE_METADATA_CHANGED".into());
                }
            } else if advertised.origin() != self.resource.origin() {
                return Err("MCP_OAUTH_EXTERNAL_METADATA_REVIEW_REQUIRED".into());
            }
            vec![advertised]
        } else if let Some(explicit) = &self.metadata_url {
            vec![explicit.clone()]
        } else {
            self.resource_candidates.clone()
        };
        let (prm, prm_url) = metadata(&client, &prm_urls).await?;
        let prm = protected_resource(&prm, &self.resource, &self.issuer, self.network)?;
        let (server, server_url) = metadata(&client, &self.server_candidates).await?;
        let server = authorization_server(&server, &self.issuer, self.network)?;
        let selected = challenge.scopes.as_ref().or(prm.scopes.as_ref());
        Ok(
            json!({"resource":self.resource.as_str(),"issuer":self.issuer,
            "protected_resource_metadata_url":prm_url.as_str(),"server_metadata_url":server_url.as_str(),
            "advertised_issuers":prm.issuers,"server_metadata":server,
            "requested_scopes":selected,"scope_source":if challenge.scopes.is_some(){"challenge"}else if prm.scopes.is_some(){"protected_resource_metadata"}else{"omit"},
            "scope_review_required":true,"untrusted":true,"browserStarted":false,"credentialsStored":false,
            "automaticReplay":false,"protocolSpecification":"2026-07-28"}),
        )
    }
}

async fn metadata(client: &reqwest::Client, urls: &[Url]) -> Result<(Value, Url), String> {
    for target in urls {
        let mut r = client
            .get(target.clone())
            .header(header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| "MCP_OAUTH_METADATA_NETWORK")?;
        if matches!(r.status().as_u16(), 404 | 405) {
            continue;
        }
        if !r.status().is_success() {
            return Err(format!("MCP_OAUTH_METADATA_HTTP_{}", r.status().as_u16()));
        }
        let content = r
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|s| s.to_str().ok())
            .ok_or("MCP_OAUTH_METADATA_CONTENT_TYPE")?;
        let mut parts = content.split(';');
        let mime = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        if !(mime == "application/json"
            || (mime.starts_with("application/") && mime.ends_with("+json")))
            || parts.any(|p| !p.trim().eq_ignore_ascii_case("charset=utf-8"))
        {
            return Err("MCP_OAUTH_METADATA_CONTENT_TYPE".into());
        }
        if r.content_length().is_some_and(|n| n > LIMIT as u64) {
            return Err("MCP_OAUTH_METADATA_LIMIT".into());
        }
        let mut bytes = vec![];
        while let Some(chunk) = r.chunk().await.map_err(|_| "MCP_OAUTH_METADATA_BODY")? {
            if bytes.len() + chunk.len() > LIMIT {
                return Err("MCP_OAUTH_METADATA_LIMIT".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        return Ok((strict_json(&bytes)?, target.clone()));
    }
    Err("MCP_OAUTH_METADATA_NOT_FOUND".into())
}

// Reject duplicate keys throughout a bounded document. A later duplicate must
// never replace an issuer/resource/capability that was reviewed earlier.
struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(json!(v)))
            }
            fn visit_none<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut values = vec![];
                while let Some(Unique(v)) = a.next_element()? {
                    values.push(v);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some((k, Unique(v))) = a.next_entry::<String, Unique>()? {
                    if values.insert(k, v).is_some() {
                        return Err(de::Error::custom("duplicate"));
                    }
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        d.deserialize_any(V)
    }
}
pub(crate) fn strict_json(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > LIMIT {
        return Err("MCP_OAUTH_METADATA_LIMIT".into());
    }
    let Unique(v) = serde_json::from_slice(bytes).map_err(|_| "MCP_OAUTH_METADATA_JSON")?;
    crate::mcp_client::schema::bounded(&v, LIMIT).map_err(|_| "MCP_OAUTH_METADATA_LIMIT")?;
    if !v.is_object() {
        return Err("MCP_OAUTH_METADATA_OBJECT".into());
    }
    Ok(v)
}
fn string_list(v: &Value, key: &str, scope: bool) -> Result<Option<Vec<String>>, String> {
    let Some(value) = v.get(key) else {
        return Ok(None);
    };
    let a = value
        .as_array()
        .filter(|a| a.len() <= 256)
        .ok_or("MCP_OAUTH_METADATA_LIST")?;
    let mut seen = BTreeSet::new();
    let mut result = vec![];
    for value in a {
        let s = value
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 4096 && !s.chars().any(char::is_control))
            .ok_or("MCP_OAUTH_METADATA_LIST")?;
        if !seen.insert(s) || (scope && !scope_token(s)) {
            return Err("MCP_OAUTH_METADATA_LIST".into());
        }
        result.push(s.into());
    }
    Ok(Some(result))
}
fn scope_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 1024
        && s.bytes()
            .all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
}
fn scopes(text: &str) -> Result<Vec<String>, String> {
    if text.is_empty() || text.len() > 8192 {
        return Err("MCP_OAUTH_SCOPE".into());
    }
    let a = text.split(' ').collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    if a.len() > 256 || a.iter().any(|s| !scope_token(s) || !seen.insert(*s)) {
        return Err("MCP_OAUTH_SCOPE".into());
    }
    Ok(a.into_iter().map(String::from).collect())
}
struct ProtectedResource {
    issuers: Vec<String>,
    scopes: Option<Vec<String>>,
}
fn protected_resource(
    v: &Value,
    resource: &Url,
    issuer: &str,
    network: Network,
) -> Result<ProtectedResource, String> {
    let received = url(
        v["resource"]
            .as_str()
            .ok_or("MCP_OAUTH_RESOURCE_METADATA")?,
        true,
    )?;
    if received != *resource {
        return Err("MCP_OAUTH_RESOURCE_MISMATCH".into());
    }
    let issuers = string_list(v, "authorization_servers", false)?
        .filter(|a| !a.is_empty())
        .ok_or("MCP_OAUTH_AUTHORIZATION_SERVERS")?;
    for value in &issuers {
        as_url(value, network)?;
    }
    if !issuers.iter().any(|s| s == issuer) {
        return Err("MCP_OAUTH_ISSUER_NOT_ADVERTISED".into());
    }
    if let Some(methods) = string_list(v, "bearer_methods_supported", false)? {
        if !methods.iter().any(|s| s == "header") {
            return Err("MCP_OAUTH_HEADER_BEARER_REQUIRED".into());
        }
    }
    Ok(ProtectedResource {
        issuers,
        scopes: string_list(v, "scopes_supported", true)?,
    })
}
fn boolean(v: &Value, key: &str) -> Result<bool, String> {
    v.get(key)
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| "MCP_OAUTH_METADATA_BOOLEAN".to_string())
        })
        .transpose()
        .map(|v| v.unwrap_or(false))
}
fn authorization_server(v: &Value, issuer: &str, network: Network) -> Result<Value, String> {
    if v["issuer"].as_str() != Some(issuer) {
        return Err("MCP_OAUTH_ISSUER_MISMATCH".into());
    }
    let endpoint = |key: &str| -> Result<String, String> {
        let s = v[key].as_str().ok_or("MCP_OAUTH_AS_ENDPOINT")?;
        as_endpoint_url(s, network)?;
        Ok(s.into())
    };
    let authorization = endpoint("authorization_endpoint")?;
    let token = endpoint("token_endpoint")?;
    let registration = v
        .get("registration_endpoint")
        .map(|_| endpoint("registration_endpoint"))
        .transpose()?;
    let pkce = string_list(v, "code_challenge_methods_supported", false)?
        .ok_or("MCP_OAUTH_PKCE_REQUIRED")?;
    if !pkce.iter().any(|s| s == "S256") {
        return Err("MCP_OAUTH_PKCE_S256_REQUIRED".into());
    }
    let responses = string_list(v, "response_types_supported", false)?
        .ok_or("MCP_OAUTH_CODE_RESPONSE_REQUIRED")?;
    if !responses.iter().any(|s| s == "code") {
        return Err("MCP_OAUTH_CODE_RESPONSE_REQUIRED".into());
    }
    let grants = string_list(v, "grant_types_supported", false)?;
    if grants
        .as_ref()
        .is_some_and(|a| !a.iter().any(|s| s == "authorization_code"))
    {
        return Err("MCP_OAUTH_CODE_GRANT_REQUIRED".into());
    }
    let auth_methods = string_list(v, "token_endpoint_auth_methods_supported", false)?;
    Ok(
        json!({"issuer":issuer,"consent_endpoint":authorization,"exchange_endpoint":token,
        "registration_endpoint":registration,"pkce_s256":true,"code_flow":true,
        "refresh_supported":grants.as_ref().is_some_and(|a|a.iter().any(|s|s=="refresh_token")),
        "public_client_supported":auth_methods.as_ref().is_some_and(|a|a.iter().any(|s|s=="none")),
        "basic_client_supported":auth_methods.as_ref().is_none_or(|a|a.iter().any(|s|s=="client_secret_basic")),
        "post_client_supported":auth_methods.as_ref().is_some_and(|a|a.iter().any(|s|s=="client_secret_post")),
        "available_scopes":string_list(v,"scopes_supported",true)?,
        "client_metadata_documents_supported":boolean(v,"client_id_metadata_document_supported")?,
        "callback_issuer_required":boolean(v,"authorization_response_iss_parameter_supported")?}),
    )
}

#[derive(Default)]
struct Challenge {
    metadata: Option<String>,
    scopes: Option<Vec<String>>,
}
fn tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}
struct HeaderParser<'a> {
    s: &'a [u8],
    i: usize,
}
type ParsedChallenge = (String, BTreeMap<String, String>);
impl HeaderParser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && b" \t".contains(&self.s[self.i]) {
            self.i += 1;
        }
    }
    fn token(&mut self) -> Result<String, String> {
        let begin = self.i;
        while self.i < self.s.len() && tchar(self.s[self.i]) {
            self.i += 1;
        }
        if begin == self.i {
            return Err("MCP_OAUTH_CHALLENGE_FORMAT".into());
        }
        Ok(String::from_utf8(self.s[begin..self.i].to_vec())
            .map_err(|_| "MCP_OAUTH_CHALLENGE_FORMAT")?)
    }
    fn value(&mut self) -> Result<String, String> {
        if self.s.get(self.i) != Some(&b'"') {
            return self.token();
        }
        self.i += 1;
        let mut v = vec![];
        while let Some(&b) = self.s.get(self.i) {
            self.i += 1;
            if b == b'"' {
                return String::from_utf8(v).map_err(|_| "MCP_OAUTH_CHALLENGE_FORMAT".into());
            }
            let b = if b == b'\\' {
                let b = *self.s.get(self.i).ok_or("MCP_OAUTH_CHALLENGE_FORMAT")?;
                self.i += 1;
                b
            } else {
                b
            };
            if !(32..=126).contains(&b) {
                return Err("MCP_OAUTH_CHALLENGE_FORMAT".into());
            }
            v.push(b);
        }
        Err("MCP_OAUTH_CHALLENGE_FORMAT".into())
    }
    fn challenges(&mut self) -> Result<Vec<ParsedChallenge>, String> {
        let mut result = vec![];
        while self.i < self.s.len() {
            self.ws();
            if self.i == self.s.len() {
                break;
            }
            if result.len() >= 32 {
                return Err("MCP_OAUTH_CHALLENGE_LIMIT".into());
            }
            if self.s.get(self.i) == Some(&b',') {
                self.i += 1;
                continue;
            }
            let scheme = self.token()?;
            let end = self.i;
            self.ws();
            if self.i == end && self.i < self.s.len() && self.s[self.i] != b',' {
                return Err("MCP_OAUTH_CHALLENGE_FORMAT".into());
            }
            let mut params = BTreeMap::new();
            if !scheme.eq_ignore_ascii_case("bearer") {
                let start = self.i;
                let mut probe = start;
                while probe < self.s.len()
                    && (self.s[probe].is_ascii_alphanumeric() || b"-._~+/".contains(&self.s[probe]))
                {
                    probe += 1;
                }
                let token_end = probe;
                while self.s.get(probe) == Some(&b'=') {
                    probe += 1;
                }
                while probe < self.s.len() && b" \t".contains(&self.s[probe]) {
                    probe += 1;
                }
                if token_end > start && (probe == self.s.len() || self.s[probe] == b',') {
                    self.i = probe;
                    result.push((scheme, params));
                    continue;
                }
            }
            loop {
                self.ws();
                if self.i == self.s.len() || self.s[self.i] == b',' {
                    break;
                }
                let start = self.i;
                let key = self.token()?;
                self.ws();
                if self.s.get(self.i) != Some(&b'=') {
                    if scheme.eq_ignore_ascii_case("bearer") {
                        return Err("MCP_OAUTH_CHALLENGE_FORMAT".into());
                    }
                    // Other schemes may carry token68, e.g. Negotiate.
                    self.i = start;
                    while self.i < self.s.len() && self.s[self.i] != b',' {
                        self.i += 1;
                    }
                    break;
                }
                self.i += 1;
                self.ws();
                let value = self.value()?;
                if params.insert(key.to_ascii_lowercase(), value).is_some() {
                    return Err("MCP_OAUTH_CHALLENGE_DUPLICATE".into());
                }
                self.ws();
                if self.i == self.s.len() {
                    break;
                }
                if self.s[self.i] != b',' {
                    return Err("MCP_OAUTH_CHALLENGE_FORMAT".into());
                }
                self.i += 1;
                self.ws();
                let next = self.i;
                if next == self.s.len() {
                    return Err("MCP_OAUTH_CHALLENGE_FORMAT".into());
                }
                self.token()?;
                self.ws();
                let is_param = self.s.get(self.i) == Some(&b'=');
                self.i = next;
                if !is_param {
                    break;
                }
            }
            result.push((scheme, params));
            if result.len() > 32 {
                return Err("MCP_OAUTH_CHALLENGE_LIMIT".into());
            }
        }
        Ok(result)
    }
}
fn challenge(headers: &header::HeaderMap) -> Result<Challenge, String> {
    let mut bearer = None;
    let mut size = 0;
    for h in headers.get_all(header::WWW_AUTHENTICATE) {
        let h = h.to_str().map_err(|_| "MCP_OAUTH_CHALLENGE_FORMAT")?;
        size += h.len();
        if size > 16384 {
            return Err("MCP_OAUTH_CHALLENGE_LIMIT".into());
        }
        for (scheme, params) in (HeaderParser {
            s: h.as_bytes(),
            i: 0,
        })
        .challenges()?
        {
            if scheme.eq_ignore_ascii_case("bearer") && bearer.replace(params).is_some() {
                return Err("MCP_OAUTH_CHALLENGE_AMBIGUOUS".into());
            }
        }
    }
    let Some(mut p) = bearer else {
        return Ok(Challenge::default());
    };
    Ok(Challenge {
        metadata: p.remove("resource_metadata"),
        scopes: p.remove("scope").map(|s| scopes(&s)).transpose()?,
    })
}

#[cfg(test)]
#[path = "mcp_oauth_tests.rs"]
mod tests;
