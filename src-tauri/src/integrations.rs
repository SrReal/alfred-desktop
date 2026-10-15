use crate::{process, runtime::Core, storage};
use serde_json::{json, Value};
use std::sync::Arc;

#[cfg(windows)]
pub(crate) fn save_lock(c: &Core) -> Result<Vec<std::fs::File>, String> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    let root = crate::asset_files::directory(&c.root).map_err(|_| "CONNECTION_SAVE_LOCK")?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .custom_flags(0x00200000)
        .open(c.root.join("connection-actions.lock"))
        .map_err(|e| {
            if matches!(e.raw_os_error(), Some(32 | 33)) {
                "CONNECTION_SAVE_BUSY"
            } else {
                "CONNECTION_SAVE_LOCK"
            }
        })?;
    let metadata = lock.metadata().map_err(|_| "CONNECTION_SAVE_LOCK")?;
    if !metadata.is_file() || metadata.len() != 0 || metadata.file_attributes() & 0x400 != 0 {
        return Err("CONNECTION_SAVE_LOCK_UNSAFE".into());
    }
    crate::asset_files::identity(&lock).map_err(|_| "CONNECTION_SAVE_LOCK_UNSAFE")?;
    Ok(vec![root, lock])
}
#[cfg(not(windows))]
pub(crate) fn save_lock(_: &Core) -> Result<Vec<std::fs::File>, String> {
    Err("WINDOWS_11_REQUIRED".into())
}

pub fn connection(c: &Core, id: &str, module: &str) -> Result<Value, String> {
    let rows = c.store.query(
        "SELECT config_json,secret_ref FROM connections WHERE id=? AND module_id=?",
        &[json!(id), json!(module)],
    )?;
    let mut config: Value = serde_json::from_str(
        rows[0]["config_json"]
            .as_str()
            .ok_or("CONNECTION_NOT_CONFIGURED")?,
    )
    .map_err(|_| "CONNECTION_CONFIG_INVALID")?;
    config["secret_ref"] = rows[0]["secret_ref"].clone();
    Ok(config)
}
pub async fn save(c: &Arc<Core>, v: &Value) -> Result<Value, String> {
    let module = v["module_id"].as_str().ok_or("CONNECTION_MODULE")?;
    if ![
        "notion",
        "drive",
        "homeassistant",
        "rest",
        "mcp",
        "database",
        "git",
    ]
    .contains(&module)
    {
        return Err("CONNECTION_MODULE_INVALID".into());
    }
    let id = v["id"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(storage::id);
    let config = v["config"].clone();
    if !config.is_object() {
        return Err("CONNECTION_CONFIG_OBJECT_REQUIRED".into());
    }
    if module == "database" {
        if uuid::Uuid::parse_str(&id)
            .map_err(|_| "DATABASE_CONNECTION_ID")?
            .to_string()
            != id
        {
            return Err("DATABASE_CONNECTION_ID".into());
        }
        crate::database::validate_config(&config)?;
        let name = v["name"].as_str().ok_or("DATABASE_CONNECTION_NAME")?;
        if name.trim().is_empty() || name.len() > 200 {
            return Err("DATABASE_CONNECTION_NAME".into());
        }
    }
    if module == "git" {
        if uuid::Uuid::parse_str(&id)
            .map_err(|_| "GITHUB_CONNECTION_ID")?
            .to_string()
            != id
        {
            return Err("GITHUB_CONNECTION_ID".into());
        }
        crate::github::validate_config(&config)?;
        let name = v["name"].as_str().ok_or("GITHUB_CONNECTION_NAME")?;
        if name.trim().is_empty() || name.len() > 200 {
            return Err("GITHUB_CONNECTION_NAME".into());
        }
    }
    let _save = save_lock(c)?;
    let existing = c.store.query(
        "SELECT id,module_id,config_json,secret_ref FROM connections WHERE id=? COLLATE NOCASE",
        &[json!(id)],
    )?;
    let rows = existing.as_array().ok_or("CONNECTION_LOOKUP")?;
    if rows.len() > 1 {
        return Err("CONNECTION_ALIAS_CONFLICT".into());
    }
    if existing[0]["module_id"]
        .as_str()
        .is_some_and(|m| m != module)
    {
        return Err("CONNECTION_MODULE_CHANGED".into());
    }
    if let Some(row) = rows.first() {
        if row["id"] != id {
            return Err("CONNECTION_ID_CASE_CHANGED".into());
        }
        if v["id"].is_null() {
            return Err("CONNECTION_ID_COLLISION".into());
        }
    } else if v["id"].is_string() {
        // Caller-supplied IDs select existing rows; only the app creates new aliases.
        return Err("CONNECTION_NOT_FOUND".into());
    }
    if module == "homeassistant" {
        crate::homeassistant::validate_config(&config)?;
        if uuid::Uuid::parse_str(&id)
            .map_err(|_| "HA_CONNECTION_ID")?
            .to_string()
            != id
        {
            return Err("HA_CONNECTION_ID".into());
        }
        if let Some(secret) = v["secret"].as_str().filter(|s| !s.is_empty()) {
            crate::homeassistant::validate_token(secret)?;
        }
    }
    if module == "notion" {
        if uuid::Uuid::parse_str(&id)
            .map_err(|_| "NOTION_CONNECTION_ID")?
            .to_string()
            != id
        {
            return Err("NOTION_CONNECTION_ID".into());
        }
        crate::notion::validate_config(&config)?;
        let name = v["name"].as_str().ok_or("NOTION_CONNECTION_NAME")?;
        if name.trim().is_empty() || name.len() > 200 {
            return Err("NOTION_CONNECTION_NAME".into());
        }
        if let Some(secret) = v["secret"].as_str().filter(|s| !s.is_empty()) {
            if secret.len() > 16384 || !secret.bytes().all(|b| (33..=126).contains(&b)) {
                return Err("NOTION_CREDENTIAL_INVALID".into());
            }
        }
    }
    for key in [
        "token",
        "password",
        "api_key",
        "refresh_token",
        "authorization",
    ] {
        if config.get(key).is_some() {
            return Err("CONNECTION_SECRET_IN_CONFIG".into());
        }
    }
    if module == "mcp" {
        crate::mcp_profile::config(&config)?;
    }
    if let Some(url) = config["base_url"].as_str() {
        let url = reqwest::Url::parse(url).map_err(|_| "CONNECTION_URL_INVALID")?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err("URL_CREDENTIALS_DENIED".into());
        }
        if url.scheme() != "https"
            && !matches!(url.host_str(), Some("localhost" | "127.0.0.1"))
            && !(module == "mcp" && url.scheme() == "http" && url.host_str() == Some("[::1]"))
            && !(module == "homeassistant"
                && config["allow_insecure_http"] == true
                && url.scheme() == "http")
        {
            return Err("CONNECTION_TLS_REQUIRED".into());
        }
    }
    let alias = if module == "drive" {
        format!("drive.credentials.{id}")
    } else {
        format!("connection.{id}")
    };
    if module == "homeassistant" {
        if let Some(row) = rows.first() {
            let old = row["config_json"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            let target_changed = row["secret_ref"] != alias
                || old.as_ref().is_none_or(|old| {
                    crate::homeassistant::validate_config(old).is_err()
                        || old["base_url"] != config["base_url"]
                });
            if target_changed && v["secret"].as_str().is_none_or(str::is_empty) {
                return Err("HA_CREDENTIAL_REQUIRED_FOR_TARGET_CHANGE".into());
            }
        }
        return crate::homeassistant::save_connection(
            c,
            &id,
            v["name"].as_str().ok_or("HA_CONNECTION_NAME")?,
            &config,
            v["secret"].as_str(),
            !rows.is_empty(),
        )
        .await;
    }
    if module == "mcp" {
        let typed = crate::mcp_profile::config(&config)?;
        let own_alias = crate::mcp_credentials::alias(&id)?;
        let expected = if let Some(wanted) = v.get("expected_profile") {
            let old: Value = rows
                .first()
                .and_then(|r| r["config_json"].as_str())
                .and_then(|s| serde_json::from_str(s).ok())
                .ok_or("MCP_PROFILE_CHANGED")?;
            if &old != wanted {
                return Err("MCP_PROFILE_CHANGED".into());
            }
            Some(crate::mcp_save::Expected {
                raw: old,
                generation: c.store.setting(&crate::mcp_save::generation_key(&id)),
                binding: c.store.setting(&crate::mcp_save::binding_key(&id)),
            })
        } else {
            None
        };
        if !crate::mcp_profile::required_secrets(&typed).is_empty()
            && rows.first().is_some_and(|r| r["secret_ref"] != own_alias)
            && v["secret"].as_str().is_none_or(str::is_empty)
        {
            return Err("MCP_CREDENTIAL_REQUIRED_FOR_PROFILE_MIGRATION".into());
        }
        return crate::mcp_save::save_connection(
            crate::mcp_save::Context {
                core: c,
                lease: &_save,
            },
            &id,
            v["name"].as_str().ok_or("MCP_CONNECTION_NAME")?,
            &config,
            v["secret"].as_str(),
            !rows.is_empty(),
            expected.as_ref(),
        )
        .await;
    }
    if module == "rest" {
        let typed = crate::rest_profile::config(&config)?;
        let own_alias = crate::rest_credentials::alias(&id)?;
        if !crate::rest_profile::required_secrets(&typed)?.is_empty()
            && rows.first().is_some_and(|r| r["secret_ref"] != own_alias)
            && v["secret"].as_str().is_none_or(str::is_empty)
        {
            return Err("REST_CREDENTIAL_REQUIRED_FOR_PROFILE_MIGRATION".into());
        }
        return crate::rest_save::save_connection(
            c,
            &id,
            v["name"].as_str().ok_or("REST_CONNECTION_NAME")?,
            &config,
            v["secret"].as_str(),
            !rows.is_empty(),
            None,
        )
        .await;
    }
    if module == "notion" {
        if let Some(row) = rows.first() {
            let old = row["config_json"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            let migration = row["secret_ref"] != alias
                || old
                    .as_ref()
                    .is_none_or(|old| crate::notion::validate_config(old).is_err());
            if migration && v["secret"].as_str().is_none_or(str::is_empty) {
                // Legacy profiles could target another origin/auth mode/alias.
                // Reconfiguration never reuses that credential implicitly.
                return Err("NOTION_CREDENTIAL_REQUIRED_FOR_PROFILE_MIGRATION".into());
            }
        }
    }
    if module == "database" && config["engine"] != "sqlite" {
        if let Some(row) = rows.first() {
            let old = row["config_json"]
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok());
            let target_changed = old.as_ref().is_none_or(|old| {
                [
                    "engine", "host", "port", "database", "user", "driver", "ssl", "ca_path",
                ]
                .iter()
                .any(|key| old[*key] != config[*key])
            });
            if target_changed && v["secret"].as_str().is_none_or(str::is_empty) {
                // A stored password is authorized for its original endpoint.
                // Never silently send it to a changed server/driver/TLS target.
                return Err("DATABASE_CREDENTIAL_REQUIRED_FOR_TARGET_CHANGE".into());
            }
        }
    }
    if module == "notion" {
        c.store.set_setting(
            &format!("notion.connection_generation.{id}"),
            &json!(storage::id()),
        )?;
    }
    // Invalidate prepared/in-flight GitHub requests before changing a credential.
    // Generations are public configuration state, never token hashes or bytes.
    if module == "git" {
        c.store.set_setting(
            &format!("github.connection_generation.{id}"),
            &json!(storage::id()),
        )?;
    }
    if module == "database" {
        c.store.set_setting(
            &format!("database.connection_generation.{id}"),
            &json!(storage::id()),
        )?;
    }
    if module == "drive" {
        crate::drive_auth::save(c, &id, v, rows.first())?;
    } else if let Some(secret) = v["secret"].as_str().filter(|s| !s.is_empty()) {
        process::native(
            &c.native,
            "secret.set",
            &json!({"alias":alias,"value":secret}),
        )
        .await?;
    }
    c.store.execute("INSERT INTO connections(id,module_id,name,config_json,secret_ref) VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,config_json=excluded.config_json,secret_ref=excluded.secret_ref",&[json!(id),json!(module),v["name"].clone(),json!(config.to_string()),json!(alias)])?;
    Ok(json!({"id":id,"live":"LIVE_NOT_RUN"}))
}
#[cfg(test)]
fn client() -> Result<reqwest::Client, String> {
    crate::http_client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| "HTTP_CLIENT".into())
}
pub fn scoped_url(base: &str, endpoint: &str) -> Result<reqwest::Url, String> {
    let path = endpoint.split('?').next().unwrap_or(endpoint);
    if !endpoint.starts_with('/')
        || endpoint.starts_with("//")
        || path.contains("..")
        || endpoint.contains('\\')
        || endpoint.contains('#')
        || path.to_lowercase().contains("%2f")
        || path.to_lowercase().contains("%2e")
    {
        return Err("HTTP_ENDPOINT_SCOPE".into());
    }
    let base = reqwest::Url::parse(base).map_err(|_| "HTTP_BASE_URL")?;
    let url = base.join(endpoint).map_err(|_| "HTTP_ENDPOINT")?;
    if base.origin() != url.origin() {
        return Err("HTTP_HOST_SCOPE".into());
    }
    Ok(url)
}
#[cfg(test)]
async fn decode(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP_STATUS_{}", status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|l| l > 8 * 1024 * 1024)
    {
        return Err("HTTP_RESPONSE_LIMIT".into());
    }
    if status == reqwest::StatusCode::NO_CONTENT {
        return Ok(json!({}));
    }
    let bytes = bounded_body(response, 8 * 1024 * 1024).await?;
    serde_json::from_slice(&bytes).map_err(|_| "HTTP_MALFORMED_JSON".into())
}
pub(crate) async fn bounded_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err("HTTP_RESPONSE_LIMIT".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "HTTP_BODY_READ")? {
        if bytes.len() + chunk.len() > limit {
            return Err("HTTP_RESPONSE_LIMIT".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
pub async fn dispatch(
    c: &Arc<Core>,
    module: &str,
    _action: &str,
    v: &Value,
) -> Result<Value, String> {
    if module == "homeassistant" {
        return Err("HA_PREPARED_DISPATCH_REQUIRED".into());
    }
    if module == "drive" {
        return Err("DRIVE_PREPARED_DISPATCH_REQUIRED".into());
    }
    if module == "rest" {
        return Err("REST_PREPARED_DISPATCH_REQUIRED".into());
    }
    if module == "mcp" {
        return Err("MCP_PREPARED_DISPATCH_REQUIRED".into());
    }
    let _config = connection(
        c,
        v["connection"].as_str().ok_or("CONNECTION_REQUIRED")?,
        module,
    )?;
    match module {
        "notion" => Err("NOTION_PREPARED_DISPATCH_REQUIRED".into()),
        "drive" => Err("DRIVE_PREPARED_DISPATCH_REQUIRED".into()),
        "homeassistant" => Err("HA_PREPARED_DISPATCH_REQUIRED".into()),
        "rest" => Err("REST_PREPARED_DISPATCH_REQUIRED".into()),
        "mcp" => Err("MCP_PREPARED_DISPATCH_REQUIRED".into()),
        _ => Err("INTEGRATION_MODULE_UNKNOWN".into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_scope() {
        assert!(scoped_url("https://api.example.com", "/v1/pages").is_ok());
        for p in [
            "https://evil.test",
            "//evil.test",
            "/../secret",
            "/%2f%2fevil.test",
        ] {
            assert!(scoped_url("https://api.example.com", p).is_err());
        }
    }
    #[tokio::test]
    async fn contract_errors_json_and_timeout() {
        use axum::{http::StatusCode, routing::get, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route(
                        "/ok",
                        get(|| async { axum::Json(json!({"data":[{"id":"fixture"}]})) }),
                    )
                    .route("/401", get(|| async { StatusCode::UNAUTHORIZED }))
                    .route("/403", get(|| async { StatusCode::FORBIDDEN }))
                    .route("/404", get(|| async { StatusCode::NOT_FOUND }))
                    .route("/429", get(|| async { StatusCode::TOO_MANY_REQUESTS }))
                    .route("/500", get(|| async { StatusCode::INTERNAL_SERVER_ERROR }))
                    .route("/bad", get(|| async { "invalid" })),
            )
            .await
            .unwrap();
        });
        let client = client().unwrap();
        assert_eq!(
            decode(
                client
                    .get(format!("http://{addr}/ok"))
                    .send()
                    .await
                    .unwrap()
            )
            .await
            .unwrap()["data"][0]["id"],
            "fixture"
        );
        for status in [401, 403, 404, 429, 500] {
            assert_eq!(
                decode(
                    client
                        .get(format!("http://{addr}/{status}"))
                        .send()
                        .await
                        .unwrap()
                )
                .await
                .unwrap_err(),
                format!("HTTP_STATUS_{status}")
            );
        }
        assert_eq!(
            decode(
                client
                    .get(format!("http://{addr}/bad"))
                    .send()
                    .await
                    .unwrap()
            )
            .await
            .unwrap_err(),
            "HTTP_MALFORMED_JSON"
        );
        server.abort();
    }
    #[tokio::test]
    async fn adapters_require_prepared_dispatch_for_scoped_providers() {
        let root = std::env::temp_dir().join(format!("ap-adapters-{}", storage::id()));
        let c = Arc::new(
            Core::new(
                root.clone(),
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
            )
            .unwrap(),
        );
        for (module, error) in [
            ("drive", "DRIVE_PREPARED_DISPATCH_REQUIRED"),
            ("homeassistant", "HA_PREPARED_DISPATCH_REQUIRED"),
            ("rest", "REST_PREPARED_DISPATCH_REQUIRED"),
        ] {
            assert_eq!(
                dispatch(&c, module, "request", &json!({}))
                    .await
                    .unwrap_err(),
                error
            );
        }
        assert!(save(&c,&json!({"module_id":"rest","name":"Rejected legacy","config":{"base_url":"https://api.example.com","auth":"none","operations":[]}})).await.is_err());
        assert_eq!(
            c.store.query("SELECT id FROM connections", &[]).unwrap(),
            json!([])
        );
        drop(c);
        std::fs::remove_dir_all(root).unwrap();
    }
}
