//! Durable per-connection catalog revisions. Provider notifications revoke
//! stale observations, not profile authority or the listener receiving them.
use crate::{runtime::Core, storage::Store};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Category {
    Tools,
    Resources,
    Prompts,
}
impl Category {
    pub(super) fn for_action(action: &str) -> Option<Self> {
        match action {
            "tools" | "call" => Some(Self::Tools),
            "resources" | "read_resource" => Some(Self::Resources),
            "prompts" | "get_prompt" => Some(Self::Prompts),
            _ => None,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Resources => "resources",
            Self::Prompts => "prompts",
        }
    }
}
fn key(connection: &str, category: Category) -> Result<String, String> {
    canonical(connection)?;
    Ok(format!("mcp.catalog.{connection}.{}", category.label()))
}
fn canonical(value: &str) -> Result<(), String> {
    let uuid = uuid::Uuid::parse_str(value).map_err(|_| "MCP_CATALOG_UUID")?;
    if uuid.is_nil() || uuid.to_string() != value {
        return Err("MCP_CATALOG_UUID".into());
    }
    Ok(())
}
fn read(store: &Store, key: &str) -> Result<(Option<String>, Value), String> {
    let rows = store
        .query("SELECT json_value FROM settings WHERE key=?", &[json!(key)])
        .map_err(|_| "MCP_CATALOG_STORAGE")?;
    let rows = rows.as_array().ok_or("MCP_CATALOG_FORMAT")?;
    if rows.is_empty() {
        return Ok((None, Value::Null));
    }
    if rows.len() != 1 {
        return Err("MCP_CATALOG_FORMAT".into());
    }
    let raw = rows[0]["json_value"].as_str().ok_or("MCP_CATALOG_FORMAT")?;
    let value: String = serde_json::from_str(raw).map_err(|_| "MCP_CATALOG_FORMAT")?;
    canonical(&value).map_err(|_| "MCP_CATALOG_FORMAT")?;
    Ok((Some(value), json!(raw)))
}
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(super) struct Fence {
    connection: String,
    category: Category,
    generation: Option<String>,
}
impl Fence {
    pub(super) fn capture(
        store: &Store,
        connection: &str,
        category: Category,
    ) -> Result<Self, String> {
        let key = key(connection, category)?;
        Ok(Self {
            connection: connection.into(),
            category,
            generation: read(store, &key)?.0,
        })
    }
    pub(super) fn check(&self, store: &Store) -> Result<(), String> {
        if read(store, &key(&self.connection, self.category)?)?.0 != self.generation {
            return Err("MCP_CATALOG_CHANGED_REVIEW_REQUIRED".into());
        }
        Ok(())
    }
    fn matches(&self, connection: &str, category: Category) -> bool {
        self.connection == connection && self.category == category
    }
}
pub(super) fn invalidate(core: &Core, connection: &str, category: Category) -> Result<(), String> {
    let key = key(connection, category)?;
    let previous = read(&core.store, &key)?.1;
    let generation = json!(crate::storage::id()).to_string();
    // The read/replace gap cannot silently overwrite a changed or corrupt row.
    let changed = core.store.execute(
        "INSERT INTO settings(key,json_value) SELECT ?1,?2 WHERE (?3 IS NULL AND NOT EXISTS(SELECT 1 FROM settings WHERE key=?1)) OR (?3 IS NOT NULL AND EXISTS(SELECT 1 FROM settings WHERE key=?1 AND json_value=?3)) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value WHERE settings.json_value=?3",
        &[json!(key), json!(generation), previous],
    ).map_err(|_| "MCP_CATALOG_STORAGE")?;
    if changed != 1 {
        return Err("MCP_CATALOG_CHANGED_REVIEW_REQUIRED".into());
    }
    core.mcp_observations
        .lock()
        .map_err(|_| "MCP_OBSERVATION_LOCK")?
        .retain(|_, observation| {
            observation
                .catalog
                .as_ref()
                .is_none_or(|fence| !fence.matches(connection, category))
        });
    Ok(())
}
