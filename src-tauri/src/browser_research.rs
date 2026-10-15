//! Anonymous public-web reads. No browser session, cookies, caller headers or
//! production private-network exception. HTML is inert, bounded source material.
use crate::{policy, runtime::Core, storage};
use dom_query::{Document, Selection};
use reqwest::{header, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

const BODY_LIMIT: usize = 256 * 1024;
const FETCH_SECONDS: u64 = 12;
pub(crate) const TOOLS: [&str; 3] = ["browser.fetch", "browser.search", "browser.research"];

fn secret_key(key: &str) -> bool {
    let key: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "password", "passwd", "secret", "token", "auth", "session", "cookie", "apikey", "csrf",
        "otp",
    ]
    .iter()
    .any(|part| key.contains(part))
        || ["key", "sid", "code", "credential", "credentials"].contains(&key.as_str())
}

fn url(raw: &str) -> Result<Url, String> {
    if raw.chars().count() > 8192 || raw.trim() != raw || raw.chars().any(char::is_control) {
        return Err("WEB_URL_INVALID".into());
    }
    let mut url = Url::parse(raw).map_err(|_| "WEB_URL_INVALID")?;
    if !["http", "https"].contains(&url.scheme())
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port() == Some(0)
    {
        return Err("WEB_URL_DENIED".into());
    }
    if url.query_pairs().any(|(key, _)| secret_key(&key)) {
        return Err("WEB_URL_SECRET_DENIED".into());
    }
    if let Some(fragment) = url.fragment() {
        let mut probe = url.clone();
        probe.set_query(Some(fragment));
        if probe.query_pairs().any(|(key, _)| secret_key(&key)) {
            return Err("WEB_URL_SECRET_DENIED".into());
        }
    }
    url.set_fragment(None);
    if url.as_str().len() > 8192 {
        return Err("WEB_URL_INVALID".into());
    }
    Ok(url)
}

// Audit must not rely on generic text redaction to recognize URL userinfo or
// encoded credential parameter names. Invalid/sensitive URLs are never echoed.
pub(crate) fn review(input: &Value) -> Value {
    let mut result = input.clone();
    if let Some(raw) = input["url"].as_str() {
        result["url"] = json!(url(raw)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| "[REDACTED]".into()));
    }
    if let Some(urls) = input["urls"].as_array() {
        result["urls"] = json!(urls
            .iter()
            .map(|raw| raw
                .as_str()
                .and_then(|s| url(s).ok())
                .map(|u| u.to_string())
                .unwrap_or_else(|| "[REDACTED]".into()))
            .collect::<Vec<_>>());
    }
    policy::redact(&result)
}

fn ipv4_public(ip: Ipv4Addr) -> bool {
    let n = u32::from(ip);
    ![
        (0, 8),
        (0x0a000000, 8),
        (0x64400000, 10),
        (0x7f000000, 8),
        (0xa9fe0000, 16),
        (0xac100000, 12),
        (0xc0000000, 24),
        (0xc0000200, 24),
        (0xc0586300, 24),
        (0xc0a80000, 16),
        (0xc6120000, 15),
        (0xc6336400, 24),
        (0xcb007100, 24),
        (0xe0000000, 4),
        (0xf0000000, 4),
    ]
    .iter()
    .any(|(base, bits)| n >> (32 - bits) == base >> (32 - bits))
}
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => ipv4_public(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return ipv4_public(v4);
            }
            let n = u128::from(v6);
            n >> 125 == 1
                && ![
                    (0x20010000000000000000000000000000u128, 23),
                    (0x20010db8000000000000000000000000, 32),
                    (0x20020000000000000000000000000000, 16),
                    (0x3fff0000000000000000000000000000, 20),
                ]
                .iter()
                .any(|(base, bits)| n >> (128 - bits) == base >> (128 - bits))
        }
    }
}
fn normalized(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}
fn hostname(url: &Url) -> Result<String, String> {
    Ok(url
        .host_str()
        .ok_or("WEB_URL_DENIED")?
        .trim_matches(['[', ']'])
        .to_owned())
}
fn public_host(host: &str) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return public_ip(ip);
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host.contains('.')
        && ![
            "localhost",
            "local",
            "internal",
            "test",
            "invalid",
            "example",
            "home.arpa",
        ]
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

struct Control<'a>(&'a Arc<Core>);
impl Control<'_> {
    fn check(&self) -> Result<(), String> {
        if !self.0.enabled("browser") {
            return Err("TOOL_DISABLED_OR_UNKNOWN".into());
        }
        if self.0.store.setting("privacy") == true {
            return Err("PRIVACY_DENIED".into());
        }
        Ok(())
    }
    async fn wait<T>(&self, future: impl Future<Output = Result<T, String>>) -> Result<T, String> {
        self.check()?;
        tokio::pin!(future);
        loop {
            tokio::select! {
                result = &mut future => { self.check()?; return result; },
                _ = tokio::time::sleep(Duration::from_millis(100)) => self.check()?,
            }
        }
    }
}

#[derive(Clone)]
enum Resolver {
    System,
    // This seam is absent from production and can only reach a test-owned origin.
    #[cfg(test)]
    OwnedFixture {
        port: u16,
    },
}
impl Resolver {
    #[cfg(test)]
    fn owned(&self, url: &Url) -> bool {
        matches!(self, Self::OwnedFixture { port } if url.scheme()=="http" && url.host_str()==Some("owned.test") && url.port()==Some(*port))
    }
    fn allowed(&self, url: &Url, ip: IpAddr) -> bool {
        #[cfg(test)]
        if self.owned(url) {
            return normalized(ip) == IpAddr::V4(Ipv4Addr::LOCALHOST);
        }
        let _ = url;
        public_ip(ip)
    }
    async fn resolve(&self, url: &Url) -> Result<Vec<SocketAddr>, String> {
        let port = url.port_or_known_default().ok_or("WEB_URL_DENIED")?;
        #[cfg(test)]
        if self.owned(url) {
            return Ok(vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)]);
        }
        let host = hostname(url)?;
        if !public_host(&host) {
            return Err("WEB_DESTINATION_DENIED".into());
        }
        let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| "WEB_DNS_FAILED")?
            .take(17)
            .collect();
        validate_addresses(&addresses, |ip| self.allowed(url, ip))?;
        Ok(addresses
            .into_iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect())
    }
}
fn validate_addresses(
    addresses: &[SocketAddr],
    allowed: impl Fn(IpAddr) -> bool,
) -> Result<(), String> {
    if addresses.is_empty()
        || addresses.len() > 16
        || addresses.iter().any(|addr| !allowed(addr.ip()))
    {
        return Err("WEB_DESTINATION_DENIED".into());
    }
    Ok(())
}
fn peer_pinned(peer: Option<SocketAddr>, addresses: &[SocketAddr]) -> bool {
    peer.is_some_and(|peer| {
        addresses.iter().any(|addr| {
            addr.port() == peer.port() && normalized(addr.ip()) == normalized(peer.ip())
        })
    })
}
fn redirect(from: &Url, location: &str) -> Result<Url, String> {
    let next = url(from
        .join(location)
        .map_err(|_| "WEB_REDIRECT_INVALID")?
        .as_str())?;
    if from.scheme() == "https" && next.scheme() != "https" {
        return Err("WEB_TLS_DOWNGRADE_DENIED".into());
    }
    Ok(next)
}

struct Fetched {
    data: Value,
    source: String,
}
fn transport_error(error: reqwest::Error, fallback: &str) -> String {
    if error.is_timeout() {
        "WEB_TIMEOUT".into()
    } else {
        fallback.into()
    }
}
async fn fetch(
    control: &Control<'_>,
    resolver: &Resolver,
    raw: &str,
    max_chars: usize,
) -> Result<Fetched, String> {
    let initial = url(raw)?;
    control
        .wait(async {
            tokio::time::timeout(
                Duration::from_secs(FETCH_SECONDS),
                fetch_inner(control, resolver, initial, max_chars),
            )
            .await
            .map_err(|_| "WEB_TIMEOUT".to_string())?
        })
        .await
}
async fn fetch_inner(
    control: &Control<'_>,
    resolver: &Resolver,
    mut current: Url,
    max_chars: usize,
) -> Result<Fetched, String> {
    let mut visited = HashSet::new();
    let requested = current.to_string();
    let mut redirects = 0;
    loop {
        control.check()?;
        if !visited.insert(current.to_string()) {
            return Err("WEB_REDIRECT_LOOP".into());
        }
        let addresses = control.wait(resolver.resolve(&current)).await?;
        let host = hostname(&current)?;
        let client = crate::http_client::builder()
            .no_proxy()
            .referer(false)
            .redirect(reqwest::redirect::Policy::none())
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .timeout(Duration::from_secs(FETCH_SECONDS))
            .connect_timeout(Duration::from_secs(5))
            .resolve_to_addrs(&host, &addresses)
            .user_agent("AlfredPet/0.1 public-research")
            .build()
            .map_err(|_| "WEB_CLIENT_FAILED")?;
        let mut response = control
            .wait(async {
                client
                    .get(current.clone())
                    .header(header::ACCEPT_ENCODING, "identity")
                    .header(
                        header::ACCEPT,
                        "text/html, text/plain, application/json, application/xml, text/xml",
                    )
                    .send()
                    .await
                    .map_err(|error| transport_error(error, "WEB_REQUEST_FAILED"))
            })
            .await?;
        if !peer_pinned(response.remote_addr(), &addresses) {
            return Err("WEB_PEER_NOT_PINNED".into());
        }
        let status = response.status();
        if [301, 302, 303, 307, 308].contains(&status.as_u16()) {
            if redirects == 5 {
                return Err("WEB_REDIRECT_LIMIT".into());
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or("WEB_REDIRECT_INVALID")?;
            current = redirect(&current, location)?;
            redirects += 1;
            continue;
        }
        if !status.is_success() {
            return Err(format!("WEB_HTTP_STATUS_{}", status.as_u16()));
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v.as_bytes() != b"identity")
        {
            return Err("WEB_CONTENT_ENCODING_DENIED".into());
        }
        let kind = content_kind(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
        )?;
        if response
            .content_length()
            .is_some_and(|n| n > BODY_LIMIT as u64)
        {
            return Err("WEB_BODY_LIMIT".into());
        }
        let mut bytes = Vec::new();
        loop {
            let next = control
                .wait(async {
                    response
                        .chunk()
                        .await
                        .map_err(|error| transport_error(error, "WEB_BODY_READ_FAILED"))
                })
                .await?;
            let Some(chunk) = next else {
                break;
            };
            if bytes.len() + chunk.len() > BODY_LIMIT {
                return Err("WEB_BODY_LIMIT".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        control.check()?;
        let source = std::str::from_utf8(&bytes)
            .map_err(|_| "WEB_UTF8_REQUIRED")?
            .trim_start_matches('\u{feff}')
            .to_owned();
        let mut data = if kind == "html" {
            html(&source, &current, max_chars)
        } else {
            let (text, truncated) = bounded(&source, max_chars);
            json!({"title":"","text":text,"truncated":truncated,"links":[],"extraction":"utf8_text"})
        };
        data["url"] = json!(current.as_str());
        data["requested_url"] = json!(requested);
        data["status"] = json!(status.as_u16());
        data["content_type"] = json!(kind);
        data["bytes"] = json!(bytes.len());
        data["sha256"] = json!(format!("{:x}", Sha256::digest(&bytes)));
        data["fetched_at"] = json!(storage::now());
        data["redirects"] = json!(redirects);
        data["untrusted_content"] = json!(true);
        data["authenticated"] = json!(false);
        control.check()?;
        return Ok(Fetched { data, source });
    }
}
fn content_kind(content_type: Option<&str>) -> Result<&'static str, String> {
    let content_type = content_type.ok_or("WEB_CONTENT_TYPE_REQUIRED")?;
    let mut parts = content_type.split(';');
    let media = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    for part in parts {
        if let Some((key, value)) = part.trim().split_once('=') {
            if key.trim().eq_ignore_ascii_case("charset")
                && !["utf-8", "utf8"]
                    .contains(&value.trim().trim_matches('"').to_ascii_lowercase().as_str())
            {
                return Err("WEB_UTF8_REQUIRED".into());
            }
        }
    }
    match media.as_str() {
        "text/html" => Ok("html"),
        "text/plain" => Ok("text"),
        "application/json" => Ok("json"),
        "application/xml" | "text/xml" => Ok("xml"),
        _ => Err("WEB_CONTENT_TYPE_DENIED".into()),
    }
}
fn bounded(raw: &str, limit: usize) -> (String, bool) {
    (
        raw.chars().take(limit).collect(),
        raw.chars().nth(limit).is_some(),
    )
}
fn clean_document(doc: &Document) {
    doc.select("head,input,textarea,select,script,style,template,noscript,[contenteditable],[hidden],[aria-hidden='true']").remove();
    // CSS classes, computed layout and dynamic JavaScript are intentionally not
    // evaluated. Only explicit hidden attributes and inline declarations apply.
    for node in doc.select("[style]").nodes() {
        if node.attr("style").is_some_and(|style| {
            style.split(';').any(|decl| {
                decl.split_once(':').is_some_and(|(key, value)| {
                    let value = value.trim().trim_end_matches("!important").trim();
                    key.trim().eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("none")
                        || key.trim().eq_ignore_ascii_case("visibility")
                            && ["hidden", "collapse"]
                                .iter()
                                .any(|v| value.eq_ignore_ascii_case(v))
                })
            })
        }) {
            node.remove_from_parent();
        }
    }
}
fn html(source: &str, base: &Url, max_chars: usize) -> Value {
    let doc = Document::from(source);
    let title = bounded(doc.select_single("title").text().trim(), 300).0;
    clean_document(&doc);
    let mut text = String::new();
    let mut chars = 0;
    let mut seen = 0;
    let mut truncated = false;
    for node in doc.root().descendants_it() {
        seen += 1;
        if seen > 10000 {
            truncated = true;
            break;
        }
        if node.is_text() {
            let raw = node.text();
            for word in raw.split_whitespace() {
                if !text.is_empty() {
                    text.push(' ');
                    chars += 1;
                }
                text.push_str(word);
                chars += word.chars().count();
                if chars > max_chars {
                    truncated = true;
                    break;
                }
            }
            if truncated {
                break;
            }
        }
    }
    let mut links = Vec::new();
    let nodes = doc.select("a[href]");
    for node in nodes.nodes().iter().take(100) {
        if let Some(target) = node
            .attr("href")
            .and_then(|v| base.join(&v).ok())
            .and_then(|u| url(u.as_str()).ok())
            .filter(|target| hostname(target).is_ok_and(|host| public_host(&host)))
        {
            links.push(json!({"text":bounded(node.text().trim(),300).0,"url":target.as_str()}));
        }
    }
    json!({"title":title,"text":bounded(&text,max_chars).0,"truncated":truncated,"links":links,
        "links_truncated":nodes.nodes().len()>100,"extraction":"inert_static_html","css_and_javascript_evaluated":false})
}

fn search_results(source: &str, provider_url: &Url, limit: usize) -> Result<Value, String> {
    let doc = Document::from(source);
    if !doc
        .select("#challenge-form,#anomaly-form,.anomaly-modal")
        .nodes()
        .is_empty()
    {
        return Err("WEB_SEARCH_PROVIDER_CHALLENGE".into());
    }
    clean_document(&doc);
    let anchors = doc.select(".result__a");
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    for node in anchors.nodes().iter().take(100) {
        let Some(href) = node.attr("href") else {
            continue;
        };
        let Ok(mut target) = provider_url.join(&href) else {
            continue;
        };
        if target
            .host_str()
            .is_some_and(|h| h == "duckduckgo.com" || h.ends_with(".duckduckgo.com"))
            && target.path() == "/l/"
        {
            let Some(raw) = target
                .query_pairs()
                .find(|(key, _)| key == "uddg")
                .map(|(_, value)| value.to_string())
            else {
                continue;
            };
            let Ok(unwrapped) = url(&raw) else {
                continue;
            };
            target = unwrapped;
        }
        let Ok(target) = url(target.as_str()) else {
            continue;
        };
        if !public_host(&hostname(&target)?) || !seen.insert(target.to_string()) {
            continue;
        }
        let title = bounded(node.text().trim(), 300).0;
        if title.is_empty() {
            continue;
        }
        let snippet = node
            .ancestors_it(Some(16))
            .find(|parent| {
                parent
                    .attr("class")
                    .is_some_and(|classes| classes.split_whitespace().any(|c| c == "result__body"))
            })
            .map(|parent| {
                bounded(
                    Selection::from(parent)
                        .select_single(".result__snippet")
                        .text()
                        .trim(),
                    400,
                )
                .0
            })
            .unwrap_or_default();
        results.push(json!({"title":title,"url":target.as_str(),"snippet":snippet}));
        if results.len() == limit {
            break;
        }
    }
    if results.is_empty()
        && doc
            .select(".no-results,.result--no-result")
            .nodes()
            .is_empty()
    {
        return Err("WEB_SEARCH_UNSUPPORTED_PROVIDER_MARKUP".into());
    }
    Ok(
        json!({"results":results,"untrusted_content":true,"provider":"duckduckgo","provider_url":provider_url.as_str()}),
    )
}
pub(crate) async fn dispatch(core: &Arc<Core>, tool: &str, input: &Value) -> Result<Value, String> {
    dispatch_with(core, tool, input, &Resolver::System).await
}
async fn dispatch_with(
    core: &Arc<Core>,
    tool: &str,
    input: &Value,
    resolver: &Resolver,
) -> Result<Value, String> {
    let control = Control(core);
    control.check()?;
    let max_chars = input["max_chars"].as_u64().unwrap_or(16000) as usize;
    if !(1..=64000).contains(&max_chars) {
        return Err("WEB_TEXT_LIMIT".into());
    }
    let output = match tool {
        "browser.fetch" => {
            fetch(
                &control,
                resolver,
                input["url"].as_str().ok_or("WEB_URL_REQUIRED")?,
                max_chars,
            )
            .await?
            .data
        }
        "browser.research" => {
            let urls = input["urls"].as_array().ok_or("WEB_URLS_REQUIRED")?;
            if urls.is_empty() || urls.len() > 5 {
                return Err("WEB_SOURCE_LIMIT".into());
            }
            let mut seen = HashSet::new();
            let mut sources = Vec::new();
            for (index, raw) in urls.iter().enumerate() {
                control.check()?;
                let raw = raw.as_str().ok_or("WEB_URL_REQUIRED")?;
                let result = match url(raw) {
                    Ok(target) if !seen.insert(target.to_string()) => {
                        Err("WEB_DUPLICATE_SOURCE".into())
                    }
                    Ok(target) => fetch(&control, resolver, target.as_str(), max_chars)
                        .await
                        .map(|f| f.data),
                    Err(error) => Err(error),
                };
                control.check()?;
                sources.push(match result {
                    Ok(data)=>json!({"index":index,"status":"succeeded","source":data}),
                    Err(error)=>json!({"index":index,"status":"failed","url":url(raw).map(|u|u.to_string()).unwrap_or_else(|_|"[REDACTED]".into()),"error":error}),
                });
            }
            json!({"sources":sources,"untrusted_content":true,"synthesis":"Caller must compare and cite the source URLs; no automatic truth verification"})
        }
        "browser.search" => {
            if input["provider"] != "duckduckgo" {
                return Err("WEB_SEARCH_PROVIDER_UNSUPPORTED".into());
            }
            let query = input["query"].as_str().ok_or("WEB_QUERY_REQUIRED")?;
            if query.trim().is_empty()
                || query.chars().count() > 500
                || query.chars().any(char::is_control)
                || policy::contains_secret(query)
            {
                return Err("WEB_QUERY_DENIED".into());
            }
            let limit = input["limit"].as_u64().unwrap_or(10) as usize;
            if !(1..=20).contains(&limit) {
                return Err("WEB_RESULT_LIMIT".into());
            }
            let mut endpoint =
                Url::parse("https://html.duckduckgo.com/html/").expect("fixed provider URL");
            endpoint.query_pairs_mut().append_pair("q", query);
            let fetched = fetch(&control, resolver, endpoint.as_str(), max_chars).await?;
            if fetched.data["content_type"] != "html" {
                return Err("WEB_SEARCH_UNSUPPORTED_PROVIDER_MARKUP".into());
            }
            let final_url = url(fetched.data["url"]
                .as_str()
                .ok_or("WEB_SEARCH_PROVIDER_URL")?)?;
            if final_url.scheme() != "https" || final_url.host_str() != Some("html.duckduckgo.com")
            {
                return Err("WEB_SEARCH_PROVIDER_REDIRECT_DENIED".into());
            }
            let mut data = search_results(&fetched.source, &final_url, limit)?;
            for key in ["fetched_at", "sha256", "bytes"] {
                data[key] = fetched.data[key].clone();
            }
            data
        }
        _ => return Err("TOOL_UNKNOWN".into()),
    };
    control.check()?;
    Ok(output)
}

#[cfg(all(test, windows))]
#[path = "browser_research_tests.rs"]
mod tests;
