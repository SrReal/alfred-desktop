//! Observed, bounded database operations; secrets only cross the native stdin pipe.
use crate::{asset_files, database_sql, integrations, policy::Risk, process, runtime::Core};
use base64::Engine;
use rusqlite::{
    hooks::{AuthAction, AuthContext, Authorization},
    limits::Limit,
    types::{Value as SqlValue, ValueRef},
    Connection, OpenFlags,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub(crate) const TOOLS: &[&str] = &[
    "database.query",
    "database.execute",
    "database.tables",
    "database.schema",
    "database.explain",
    "database.export",
    "database.import",
];
const ROWS: usize = 5000;
const BYTES: usize = 4 * 1024 * 1024;
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn default_read() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    engine: String,
    #[serde(default = "default_read")]
    read_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    database: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    driver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ssl: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ca_path: Option<String>,
}
fn profile(value: &Value) -> Result<Profile, String> {
    let p: Profile =
        serde_json::from_value(value.clone()).map_err(|_| "DATABASE_CONFIG_INVALID")?;
    if p.engine == "sqlite" {
        if p.path
            .as_ref()
            .is_none_or(|s| s.is_empty() || s.len() > 4000 || s.contains('\0'))
            || p.host.is_some()
            || p.port.is_some()
            || p.database.is_some()
            || p.user.is_some()
            || p.driver.is_some()
            || p.ssl.is_some()
            || p.ca_path.is_some()
        {
            return Err("DATABASE_CONFIG_INVALID".into());
        }
    } else {
        if !["mysql", "mariadb", "postgresql", "sqlserver"].contains(&p.engine.as_str())
            || p.path.is_some()
            || p.port.is_none_or(|n| n == 0)
        {
            return Err("DATABASE_CONFIG_INVALID".into());
        }
        for s in [&p.host, &p.database, &p.user, &p.driver] {
            if s.as_ref()
                .is_none_or(|s| s.is_empty() || s.len() > 200 || s.chars().any(char::is_control))
            {
                return Err("DATABASE_CONFIG_INVALID".into());
            }
        }
        if !p
            .host
            .as_ref()
            .unwrap()
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-:".contains(c))
        {
            return Err("DATABASE_HOST_INVALID".into());
        }
        if !matches!(
            p.ssl.as_deref(),
            Some("verify_full" | "require" | "disable")
        ) {
            return Err("DATABASE_SSL_REQUIRED".into());
        }
        if p.engine == "sqlserver" {
            if p.driver.as_deref() != Some("ODBC Driver 18 for SQL Server") || p.ca_path.is_some() {
                return Err("DATABASE_DRIVER_OR_CA_UNSUPPORTED".into());
            }
        } else {
            let driver = p.driver.as_deref().unwrap();
            let supported = match p.engine.as_str() {
                "mysql" => driver
                    .strip_prefix("MySQL ODBC ")
                    .and_then(|s| s.strip_suffix(" Unicode Driver"))
                    .is_some_and(|v| {
                        v.split('.')
                            .next()
                            .and_then(|s| s.parse::<u32>().ok())
                            .is_some_and(|n| (8..=29).contains(&n))
                            && v.contains('.')
                            && v.bytes().all(|c| c.is_ascii_digit() || c == b'.')
                    }),
                "mariadb" => matches!(
                    driver,
                    "MariaDB ODBC 3.1 Driver" | "MariaDB ODBC 3.2 Driver"
                ),
                "postgresql" => matches!(driver, "PostgreSQL Unicode(x64)" | "PostgreSQL Unicode"),
                _ => false,
            };
            if !supported {
                return Err("DATABASE_DRIVER_UNSUPPORTED".into());
            }
            if p.engine == "mariadb" && p.ssl.as_deref() == Some("disable") {
                return Err("DATABASE_MARIADB_TLS_DISABLE_UNSUPPORTED".into());
            }
            if p.ssl.as_deref() == Some("verify_full")
                && p.ca_path
                    .as_ref()
                    .is_none_or(|s| s.is_empty() || s.len() > 4000)
            {
                return Err("DATABASE_CA_REQUIRED".into());
            }
            if p.ssl.as_deref() != Some("verify_full") && p.ca_path.is_some() {
                return Err("DATABASE_CA_MODE".into());
            }
        }
    }
    Ok(p)
}
pub(crate) fn validate_config(value: &Value) -> Result<(), String> {
    profile(value).map(|_| ())
}
pub(crate) fn classify(tool: &str, value: &Value) -> Risk {
    if tool == "database.import" || tool == "database.export" {
        return Risk::Contextual;
    }
    if matches!(tool, "database.schema" | "database.tables") {
        return Risk::Free;
    }
    database_sql::analyze(
        value["engine"].as_str().unwrap_or("sqlite"),
        value["sql"].as_str().unwrap_or(""),
    )
    .unwrap_or(Risk::Mandatory)
}
pub(crate) fn schema(tool: &str) -> Value {
    let mut props = serde_json::Map::new();
    props.insert(
        "connection".into(),
        json!({"type":"string","format":"uuid","pattern":"^[a-f0-9-]{36}$"}),
    );
    let mut required = vec!["connection"];
    if matches!(
        tool,
        "database.query" | "database.execute" | "database.explain" | "database.export"
    ) {
        props.insert(
            "sql".into(),
            json!({"type":"string","minLength":1,"maxLength":65536}),
        );
        required.push("sql");
        props.insert("parameters".into(), json!({"type":"array","maxItems":128,"items":{"anyOf":[{"type":["string","number","boolean","null"],"maxLength":65536},{"type":"object","required":["blob_base64"],"properties":{"blob_base64":{"type":"string","maxLength":87384}},"additionalProperties":false}]}}));
    }
    if matches!(
        tool,
        "database.query" | "database.explain" | "database.export"
    ) {
        props.insert(
            "limit".into(),
            json!({"type":"integer","minimum":1,"maximum":5000}),
        );
    }
    if matches!(tool, "database.execute" | "database.import") {
        props.insert(
            "fingerprint".into(),
            json!({"type":"string","pattern":"^[a-f0-9]{64}$"}),
        );
        required.push("fingerprint");
    }
    if matches!(tool, "database.export" | "database.import") {
        props.insert(
            "path".into(),
            json!({"type":"string","minLength":1,"maxLength":4000}),
        );
        required.push("path");
    }
    if tool == "database.import" {
        props.insert("table".into(),json!({"type":"string","minLength":1,"maxLength":128,"pattern":"^[A-Za-z_][A-Za-z0-9_]*$"}));
        required.push("table");
    }
    json!({"type":"object","required":required,"properties":props,"additionalProperties":false})
}
fn scalar(v: &Value) -> Result<SqlValue, String> {
    Ok(match v {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                SqlValue::Integer(i)
            } else if n.is_u64() {
                return Err("DATABASE_INTEGER_RANGE".into());
            } else {
                SqlValue::Real(
                    n.as_f64()
                        .filter(|n| n.is_finite())
                        .ok_or("DATABASE_NUMBER")?,
                )
            }
        }
        Value::String(s) if s.len() <= 65536 => SqlValue::Text(s.clone()),
        Value::Object(m) if m.len() == 1 && m.contains_key("blob_base64") => {
            let b = base64::engine::general_purpose::STANDARD
                .decode(
                    v["blob_base64"]
                        .as_str()
                        .filter(|s| s.len() <= 87384)
                        .ok_or("DATABASE_BLOB")?,
                )
                .map_err(|_| "DATABASE_BLOB")?;
            if b.len() > 65536 {
                return Err("DATABASE_CELL_LIMIT".into());
            }
            SqlValue::Blob(b)
        }
        _ => return Err("DATABASE_CELL_INVALID".into()),
    })
}
fn parameters(value: &Value) -> Result<Vec<SqlValue>, String> {
    if value.is_null() {
        return Ok(vec![]);
    }
    let values = value
        .as_array()
        .filter(|v| v.len() <= 128)
        .ok_or("DATABASE_PARAMETERS")?;
    values.iter().map(scalar).collect()
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rows {
    format: String,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    incomplete: bool,
}
fn rows(bytes: &[u8]) -> Result<Rows, String> {
    if bytes.len() > BYTES {
        return Err("DATABASE_IMPORT_LIMIT".into());
    }
    let data: Rows = serde_json::from_slice(bytes).map_err(|_| "DATABASE_IMPORT_FORMAT")?;
    if data.format != "alfredpet.database.rows.v1"
        || data.incomplete
        || data.rows.is_empty()
        || data.rows.len() > ROWS
        || data.columns.is_empty()
        || data.columns.len() > 128
    {
        return Err("DATABASE_IMPORT_FORMAT".into());
    }
    let mut names = std::collections::HashSet::new();
    for name in &data.columns {
        if !identifier(name) || !names.insert(name.to_ascii_lowercase()) {
            return Err("DATABASE_IMPORT_COLUMNS".into());
        }
    }
    for row in &data.rows {
        if row.len() != data.columns.len() {
            return Err("DATABASE_IMPORT_COLUMNS".into());
        }
        for v in row {
            scalar(v)?;
        }
    }
    Ok(data)
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .enumerate()
            .all(|(i, c)| c.is_ascii_alphabetic() || c == b'_' || (i > 0 && c.is_ascii_digit()))
}
fn quote(engine: &str, s: &str) -> Result<String, String> {
    if !identifier(s) {
        return Err("DATABASE_IDENTIFIER".into());
    }
    Ok(match engine {
        "mysql" | "mariadb" => format!("`{s}`"),
        "sqlserver" => format!("[{s}]"),
        _ => format!("\"{s}\""),
    })
}
struct Lease {
    path: PathBuf,
    file: File,
    identity: String,
    shared: bool,
}
fn parents(path: &Path, leases: &mut Vec<Lease>) -> Result<(), String> {
    for p in path.parent().ok_or("DATABASE_PARENT")?.ancestors() {
        let file = asset_files::directory(p)?;
        let identity = asset_files::identity(&file)?;
        leases.push(Lease {
            path: p.into(),
            file,
            identity,
            shared: true,
        });
    }
    Ok(())
}
#[cfg(windows)]
fn db_lease(path: &Path) -> Result<File, String> {
    use std::os::windows::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .custom_flags(0x00200000)
        .open(path)
        .map_err(|_| "DATABASE_FILE_LEASE")?;
    if !file.metadata().map_err(|_| "DATABASE_FILE")?.is_file() {
        return Err("DATABASE_FILE".into());
    }
    asset_files::identity(&file)?;
    Ok(file)
}
#[cfg(not(windows))]
fn db_lease(_: &Path) -> Result<File, String> {
    Err("WINDOWS_11_REQUIRED".into())
}
struct Scope {
    core: Arc<Core>,
    roots: Vec<PathBuf>,
    id: String,
    record: Value,
    generation: Value,
    leases: Vec<Lease>,
}
fn record(c: &Core, id: &str) -> Result<Value, String> {
    let rows = c.store.query("SELECT id,module_id,name,config_json,secret_ref FROM connections WHERE id=? COLLATE NOCASE", &[json!(id)])?;
    let rows = rows.as_array().ok_or("DATABASE_CONNECTION")?;
    if rows.len() != 1
        || rows[0]["id"] != id
        || rows[0]["module_id"] != "database"
        || rows[0]["secret_ref"] != format!("connection.{id}")
    {
        return Err("DATABASE_CONNECTION_ALIAS".into());
    }
    Ok(rows[0].clone())
}
impl Scope {
    fn check(&self) -> Result<(), String> {
        if !self.core.enabled("database") || self.core.store.setting("privacy") == true {
            return Err("DATABASE_SCOPE_DENIED".into());
        }
        if self.core.roots() != self.roots
            || record(&self.core, &self.id)? != self.record
            || self
                .core
                .store
                .setting(&format!("database.connection_generation.{}", self.id))
                != self.generation
        {
            return Err("DATABASE_CONNECTION_CHANGED".into());
        }
        for lease in &self.leases {
            let current = if lease
                .file
                .metadata()
                .map_err(|_| "DATABASE_IDENTITY")?
                .is_dir()
            {
                asset_files::directory(&lease.path)?
            } else if lease.shared {
                db_lease(&lease.path)?
            } else {
                asset_files::open(&lease.path, false)?
            };
            if asset_files::identity(&current)? != lease.identity
                || asset_files::identity(&lease.file)? != lease.identity
            {
                return Err("DATABASE_IDENTITY_CHANGED".into());
            }
            if lease.shared
                && !lease
                    .file
                    .metadata()
                    .map_err(|_| "DATABASE_IDENTITY")?
                    .is_dir()
            {
                // SQLite may create/delete adjacent WAL/SHM/journal files itself.
                // Inspect existing sidecars at every guard; never keep a deny-
                // delete lease which would break its legitimate commit cleanup.
                for suffix in ["-wal", "-shm", "-journal"] {
                    let mut name = lease.path.as_os_str().to_os_string();
                    name.push(suffix);
                    let sidecar = PathBuf::from(name);
                    match std::fs::symlink_metadata(&sidecar) {
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(_) => return Err("DATABASE_SIDECAR_ACCESS".into()),
                        Ok(_) => {
                            self.core
                                .path(sidecar.to_str().ok_or("DATABASE_SIDECAR_PATH")?, false)?;
                            db_lease(&sidecar).map_err(|_| "DATABASE_SIDECAR_UNSAFE")?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
pub(crate) struct Prepared {
    scope: Arc<Scope>,
    profile: Profile,
    tool: String,
    input: Value,
    schema: Value,
    fingerprint: String,
    db_path: Option<PathBuf>,
    data_path: Option<PathBuf>,
    import: Option<Rows>,
    import_hash: Option<String>,
    pub(crate) risk: Risk,
}
impl Prepared {
    pub(crate) fn review(&self) -> Value {
        json!({"connection":self.scope.id,"name":self.scope.record["name"],"engine":self.profile.engine,"read_only":self.profile.read_only,"host":self.profile.host,"port":self.profile.port,"database":self.profile.database,"ssl":self.profile.ssl,"path":self.data_path,"database_path":self.db_path,"sql":self.input["sql"],"parameters":self.input["parameters"],"schema":self.schema,"schema_fingerprint":self.fingerprint,"table":self.input["table"],"import_sha256":self.import_hash,"rows":self.import.as_ref().map(|d|d.rows.len()),"columns":self.import.as_ref().map(|d|&d.columns),"effects":{"transactional_sqlite":true,"remote_ddl_atomic":false,"concurrent_data_writers_isolated":false,"server_provider_code_sandboxed":false,"export_overwrite":false}})
    }
}
fn add_lease(scope: &mut Scope, path: &Path, shared: bool) -> Result<(), String> {
    parents(path, &mut scope.leases)?;
    let file = if shared {
        db_lease(path)?
    } else {
        asset_files::open(path, false)?
    };
    let identity = asset_files::identity(&file)?;
    scope.leases.push(Lease {
        path: path.into(),
        file,
        identity,
        shared,
    });
    Ok(())
}
pub(crate) async fn prepare(c: &Arc<Core>, tool: &str, input: &Value) -> Result<Prepared, String> {
    let id = input["connection"]
        .as_str()
        .ok_or("DATABASE_CONNECTION_ID")?;
    if uuid::Uuid::parse_str(id)
        .map_err(|_| "DATABASE_CONNECTION_ID")?
        .to_string()
        != id
    {
        return Err("DATABASE_CONNECTION_ID".into());
    }
    let snapshot = {
        let _lock = integrations::save_lock(c)?;
        record(c, id)?
    };
    let p = profile(
        &serde_json::from_str::<Value>(
            snapshot["config_json"]
                .as_str()
                .ok_or("DATABASE_CONFIG_INVALID")?,
        )
        .map_err(|_| "DATABASE_CONFIG_INVALID")?,
    )?;
    let mut scope = Scope {
        core: c.clone(),
        roots: c.roots(),
        id: id.into(),
        record: snapshot,
        generation: c
            .store
            .setting(&format!("database.connection_generation.{id}")),
        leases: vec![],
    };
    let mut risk = Risk::Free;
    if input.get("sql").is_some() {
        let sql_risk =
            database_sql::analyze(&p.engine, input["sql"].as_str().ok_or("DATABASE_SQL")?)?;
        parameters(&input["parameters"])?;
        if tool == "database.execute" {
            if sql_risk == Risk::Free {
                return Err("DATABASE_USE_QUERY".into());
            }
            risk = sql_risk;
        } else if sql_risk != Risk::Free {
            return Err("DATABASE_READ_STATEMENT_REQUIRED".into());
        }
    }
    if tool == "database.import" {
        risk = Risk::Contextual;
        quote(&p.engine, input["table"].as_str().ok_or("DATABASE_TABLE")?)?;
    }
    if p.read_only && matches!(tool, "database.execute" | "database.import") {
        return Err("DATABASE_READ_ONLY".into());
    }
    let db_path = if let Some(path) = &p.path {
        let path = c.path(path, false)?;
        if path.starts_with(
            c.root
                .canonicalize()
                .map_err(|_| "DATABASE_INTERNAL_PROFILE")?,
        ) {
            return Err("DATABASE_INTERNAL_PROFILE_DENIED".into());
        }
        add_lease(&mut scope, &path, true)?;
        Some(path)
    } else {
        None
    };
    if let Some(ca) = &p.ca_path {
        let path = c.path(ca, false)?;
        add_lease(&mut scope, &path, false)?;
    }
    let mut import = None;
    let mut import_hash = None;
    let data_path = if matches!(tool, "database.export" | "database.import") {
        let path = c.path(
            input["path"].as_str().ok_or("DATABASE_PATH")?,
            tool == "database.export",
        )?;
        if tool == "database.export" {
            if path.exists() {
                return Err("DATABASE_EXPORT_EXISTS".into());
            }
            parents(&path, &mut scope.leases)?;
            risk = Risk::Contextual;
        } else {
            add_lease(&mut scope, &path, false)?;
            let file = scope.leases.last_mut().ok_or("DATABASE_IMPORT_FILE")?;
            if file
                .file
                .metadata()
                .map_err(|_| "DATABASE_IMPORT_FILE")?
                .len()
                > BYTES as u64
            {
                return Err("DATABASE_IMPORT_LIMIT".into());
            }
            let mut bytes = Vec::new();
            (&mut file.file)
                .take(BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "DATABASE_IMPORT_READ")?;
            import = Some(rows(&bytes)?);
            import_hash = Some(hash(&bytes));
        }
        Some(path)
    } else {
        None
    };
    scope.check()?;
    let scope = Arc::new(scope);
    let meta = if let Some(path) = &db_path {
        let path = path.clone();
        let s = scope.clone();
        tokio::task::spawn_blocking(move || {
            s.check()?;
            let db = open_db(&path, false)?;
            db.execute_batch("BEGIN")
                .map_err(|_| "DATABASE_TRANSACTION")?;
            sqlite_schema(&db)
        })
        .await
        .map_err(|_| "DATABASE_WORKER")??
    } else {
        remote(&scope, &p, "schema", &json!({}), false).await?
    };
    scope.check()?;
    let fingerprint = hash(meta.to_string().as_bytes());
    if p.engine == "sqlite"
        && matches!(tool, "database.execute" | "database.import")
        && meta["objects"]["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|r| r[1] == "trigger"))
    {
        // Trigger bodies can broaden an apparently contextual INSERT/UPDATE.
        // Their exact definitions are part of the observation and human review.
        risk = Risk::Mandatory;
    }
    if matches!(tool, "database.execute" | "database.import") && input["fingerprint"] != fingerprint
    {
        return Err("DATABASE_SCHEMA_OBSERVATION_CHANGED".into());
    }
    Ok(Prepared {
        scope,
        profile: p,
        tool: tool.into(),
        input: input.clone(),
        schema: meta,
        fingerprint,
        db_path,
        data_path,
        import,
        import_hash,
        risk,
    })
}
fn open_db(path: &Path, write: bool) -> Result<Connection, String> {
    let flags = if write {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    } else {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    };
    let db = Connection::open_with_flags(path, flags).map_err(|_| "DATABASE_OPEN")?;
    db.busy_timeout(Duration::from_millis(500))
        .map_err(|_| "DATABASE_OPEN")?;
    db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 1024 * 1024)
        .map_err(|_| "DATABASE_LIMITS")?;
    db.set_limit(
        Limit::SQLITE_LIMIT_SQL_LENGTH,
        database_sql::SQL_LIMIT as i32,
    )
    .map_err(|_| "DATABASE_LIMITS")?;
    db.set_limit(Limit::SQLITE_LIMIT_COLUMN, 128)
        .map_err(|_| "DATABASE_LIMITS")?;
    db.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 32)
        .map_err(|_| "DATABASE_LIMITS")?;
    db.set_limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 128)
        .map_err(|_| "DATABASE_LIMITS")?;
    db.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA foreign_keys=ON;")
        .map_err(|_| "DATABASE_OPEN")?;
    if !write {
        db.execute_batch("PRAGMA query_only=ON;")
            .map_err(|_| "DATABASE_OPEN")?;
    }
    Ok(db)
}
fn sqlite_schema(db: &Connection) -> Result<Value, String> {
    let tables = read(db,"SELECT name AS table_name,type AS table_type,sql AS definition FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' AND type IN ('table','view','trigger','index') ORDER BY type,name",&[],ROWS)?;
    let columns = read(db,"SELECT m.name AS table_name,p.name AS column_name,p.type AS data_type,p.pk AS primary_key,p.hidden AS hidden,p.\"notnull\" AS not_null,p.dflt_value AS column_default FROM sqlite_schema m JOIN pragma_table_xinfo(m.name) p WHERE m.type IN ('table','view') AND m.name NOT LIKE 'sqlite_%' ORDER BY m.name,p.cid",&[],ROWS)?;
    if tables["incomplete"] == true || columns["incomplete"] == true {
        return Err("DATABASE_SCHEMA_LIMIT".into());
    }
    Ok(json!({"objects":tables,"columns":columns}))
}
fn cell(value: ValueRef<'_>) -> Result<Value, String> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(n) if n.is_finite() => json!(n),
        ValueRef::Real(_) => return Err("DATABASE_NUMBER".into()),
        ValueRef::Text(s) if s.len() <= 65536 => {
            json!(std::str::from_utf8(s).map_err(|_| "DATABASE_TEXT_UTF8")?)
        }
        ValueRef::Blob(b) if b.len() <= 65536 => {
            json!({"blob_base64":base64::engine::general_purpose::STANDARD.encode(b)})
        }
        _ => return Err("DATABASE_CELL_LIMIT".into()),
    })
}
fn read(db: &Connection, sql: &str, params: &[SqlValue], limit: usize) -> Result<Value, String> {
    let mut stmt = db.prepare(sql).map_err(|_| "DATABASE_PREPARE")?;
    if !stmt.readonly() {
        return Err("DATABASE_READ_STATEMENT_REQUIRED".into());
    }
    let names = stmt
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    if names.len() > 128 {
        return Err("DATABASE_COLUMN_LIMIT".into());
    }
    let mut cursor = stmt
        .query(rusqlite::params_from_iter(params.iter()))
        .map_err(|_| "DATABASE_PARAMETERS")?;
    let mut data: Vec<Vec<Value>> = vec![];
    let mut size = 0;
    let mut incomplete = false;
    while let Some(row) = cursor.next().map_err(|_| "DATABASE_QUERY")? {
        if data.len() == limit {
            incomplete = true;
            break;
        }
        let values = (0..names.len())
            .map(|i| cell(row.get_ref(i).map_err(|_| "DATABASE_CELL")?))
            .collect::<Result<Vec<_>, _>>()?;
        size += serde_json::to_vec(&values)
            .map_err(|_| "DATABASE_SERIALIZE")?
            .len();
        if size > BYTES - 65536 {
            return Err("DATABASE_OUTPUT_LIMIT".into());
        }
        data.push(values);
    }
    Ok(
        json!({"format":"alfredpet.database.rows.v1","columns":names,"rows":data,"incomplete":incomplete}),
    )
}
fn authorize(db: &Connection, write: bool) -> Result<(), String> {
    db.authorizer(Some(move |ctx: AuthContext<'_>| match ctx.action {
        AuthAction::Attach { .. }
        | AuthAction::Detach { .. }
        | AuthAction::Pragma { .. }
        | AuthAction::CreateVtable { .. }
        | AuthAction::DropVtable { .. }
        | AuthAction::Unknown { .. }
        | AuthAction::CreateTrigger { .. }
        | AuthAction::CreateTempTrigger { .. } => Authorization::Deny,
        AuthAction::Function { function_name }
            if write
                && ctx.accessor.is_none()
                && [
                    "sqlite_rename_table",
                    "sqlite_rename_column",
                    "sqlite_rename_test",
                    "sqlite_drop_column",
                    "sqlite_rename_quotefix",
                ]
                .contains(&function_name) =>
        {
            Authorization::Allow
        }
        AuthAction::Function { function_name } if !database_sql::function(function_name) => {
            Authorization::Deny
        }
        AuthAction::Read { .. }
        | AuthAction::Select
        | AuthAction::Function { .. }
        | AuthAction::Recursive => Authorization::Allow,
        _ if write => Authorization::Allow,
        _ => Authorization::Deny,
    }))
    .map_err(|_| "DATABASE_AUTHORIZER".into())
}
struct Cancel(Arc<AtomicBool>);
impl Drop for Cancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
fn sqlite_execute(p: Prepared, cancel: Arc<AtomicBool>) -> Result<Value, String> {
    let _lock = integrations::save_lock(&p.scope.core)?;
    p.scope.check()?;
    let write = matches!(p.tool.as_str(), "database.execute" | "database.import");
    let db = open_db(p.db_path.as_ref().ok_or("DATABASE_PATH")?, write)?;
    let started = Instant::now();
    let guard_scope = p.scope.clone();
    let interrupted = cancel.clone();
    db.progress_handler(
        2000,
        Some(move || {
            interrupted.load(Ordering::Acquire)
                || started.elapsed() > Duration::from_secs(45)
                || guard_scope.check().is_err()
        }),
    )
    .map_err(|_| "DATABASE_PROGRESS_HANDLER")?;
    // BEGIN IMMEDIATE prevents local DDL/data writers after the schema recheck.
    if write {
        db.execute_batch("BEGIN IMMEDIATE")
            .map_err(|_| "DATABASE_TRANSACTION")?;
    } else {
        db.execute_batch("BEGIN")
            .map_err(|_| "DATABASE_TRANSACTION")?;
    }
    if sqlite_schema(&db)? != p.schema {
        return Err("DATABASE_SCHEMA_OBSERVATION_CHANGED".into());
    }
    if matches!(p.tool.as_str(), "database.schema" | "database.tables") {
        return Ok(
            json!({"engine":"sqlite","schema":p.schema,"fingerprint":p.fingerprint,"read_only":p.profile.read_only}),
        );
    }
    authorize(&db, write)?;
    let result = if let Some(data) = &p.import {
        let names = data
            .columns
            .iter()
            .map(|s| quote("sqlite", s))
            .collect::<Result<Vec<_>, _>>()?
            .join(",");
        let placeholders = vec!["?"; data.columns.len()].join(",");
        let sql = format!(
            "INSERT INTO {} ({names}) VALUES ({placeholders})",
            quote("sqlite", p.input["table"].as_str().ok_or("DATABASE_TABLE")?)?
        );
        let mut stmt = db.prepare(&sql).map_err(|_| "DATABASE_IMPORT_PREPARE")?;
        let mut changed = 0;
        for row in &data.rows {
            if cancel.load(Ordering::Acquire) {
                return Err("DATABASE_CANCELLED".into());
            }
            p.scope.check()?;
            let params = row.iter().map(scalar).collect::<Result<Vec<_>, _>>()?;
            changed += stmt
                .execute(rusqlite::params_from_iter(params.iter()))
                .map_err(|_| "DATABASE_IMPORT_ROLLED_BACK")?;
        }
        json!({"changed":changed,"import_sha256":p.import_hash,"transaction":"committed"})
    } else {
        let sql = p.input["sql"].as_str().ok_or("DATABASE_SQL")?;
        let params = parameters(&p.input["parameters"])?;
        if write {
            let changed = db
                .execute(sql, rusqlite::params_from_iter(params.iter()))
                .map_err(|_| "DATABASE_EXECUTE_ROLLED_BACK")?;
            json!({"changed":changed,"transaction":"committed"})
        } else {
            let sql = if p.tool == "database.explain" {
                format!("EXPLAIN QUERY PLAN {sql}")
            } else {
                sql.into()
            };
            read(
                &db,
                &sql,
                &params,
                p.input["limit"].as_u64().unwrap_or(500) as usize,
            )?
        }
    };
    p.scope.check()?;
    if cancel.load(Ordering::Acquire) || started.elapsed() > Duration::from_secs(45) {
        return Err("DATABASE_CANCELLED".into());
    }
    if write {
        // Transaction control is internal only. The parsed user statement cannot
        // COMMIT/ROLLBACK/ATTACH or turn off the authorizer.
        db.authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .map_err(|_| "DATABASE_AUTHORIZER")?;
        db.execute_batch("COMMIT")
            .map_err(|_| "DATABASE_WRITE_OUTCOME_UNCERTAIN")?;
    }
    finish(&p, result)
}
fn finish(p: &Prepared, result: Value) -> Result<Value, String> {
    if p.tool != "database.export" {
        return Ok(result);
    }
    let bytes = serde_json::to_vec(&result).map_err(|_| "DATABASE_EXPORT_SERIALIZE")?;
    if bytes.len() > BYTES {
        return Err("DATABASE_EXPORT_LIMIT".into());
    }
    let path = p.data_path.as_ref().ok_or("DATABASE_PATH")?;
    p.scope.check()?;
    let mut file = asset_files::create(path)?;
    if file
        .write_all(&bytes)
        .and_then(|_| file.sync_all())
        .is_err()
    {
        let _ = asset_files::erase(&file);
        return Err("DATABASE_EXPORT_WRITE".into());
    }
    if let Err(e) = p.scope.check() {
        let _ = asset_files::erase(&file);
        return Err(e);
    }
    Ok(
        json!({"path":path,"sha256":hash(&bytes),"bytes":bytes.len(),"rows":result["rows"].as_array().map(|r|r.len()),"incomplete":result["incomplete"],"format":"alfredpet.database.rows.v1"}),
    )
}
async fn remote(
    scope: &Arc<Scope>,
    p: &Profile,
    action: &str,
    input: &Value,
    write: bool,
) -> Result<Value, String> {
    scope.check()?;
    let mut request = serde_json::to_value(p).map_err(|_| "DATABASE_PROFILE")?;
    let secret = process::native(
        &scope.core.native,
        "secret.get",
        &json!({"alias":format!("connection.{}",scope.id)}),
    )
    .await?;
    if secret["value"]
        .as_str()
        .is_none_or(|s| s.is_empty() || s.len() > 16384)
    {
        return Err("DATABASE_SECRET_REQUIRED".into());
    }
    scope.check()?;
    request["secret"] = secret["value"].clone();
    request["action"] = json!(action);
    request["request"] = input.clone();
    request["write"] = json!(write);
    let helper = scope
        .core
        .native
        .parent()
        .ok_or("DATABASE_NATIVE_PATH")?
        .join("database.ps1");
    let cancel = tokio_util::sync::CancellationToken::new();
    let operation = Box::pin(process::native_cancellable(
        &helper,
        "database.odbc",
        &request,
        cancel.clone(),
    ));
    tokio::pin!(operation);
    let value = loop {
        tokio::select! {
            result = &mut operation => break result,
            _ = tokio::time::sleep(Duration::from_millis(50)) => if let Err(error) = scope.check() {
                cancel.cancel(); let _ = operation.await;
                return Err(if write { format!("DATABASE_WRITE_OUTCOME_UNCERTAIN: {error}") } else { error });
            }
        }
    }.map_err(|e| if write { format!("DATABASE_WRITE_OUTCOME_UNCERTAIN: {e}") } else { e })?;
    scope.check().map_err(|e| {
        if write {
            format!("DATABASE_WRITE_OUTCOME_UNCERTAIN: {e}")
        } else {
            e
        }
    })?;
    if serde_json::to_vec(&value)
        .map_err(|_| "DATABASE_OUTPUT")?
        .len()
        > BYTES
    {
        return Err("DATABASE_OUTPUT_LIMIT".into());
    }
    Ok(value)
}
pub(crate) async fn execute(p: Prepared) -> Result<Value, String> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let _cancel = Cancel(cancelled.clone());
    if p.db_path.is_some() {
        return tokio::task::spawn_blocking(move || sqlite_execute(p, cancelled))
            .await
            .map_err(|_| "DATABASE_WORKER")?;
    }
    let _lock = integrations::save_lock(&p.scope.core)?;
    p.scope.check()?;
    if matches!(p.tool.as_str(), "database.schema" | "database.tables") {
        return Ok(
            json!({"engine":p.profile.engine,"schema":p.schema,"fingerprint":p.fingerprint,"read_only":p.profile.read_only}),
        );
    }
    let mut input = p.input.clone();
    input["expected_schema"] = p.schema.clone();
    if let Some(data) = &p.import {
        input["data"] = serde_json::to_value(data).map_err(|_| "DATABASE_IMPORT_FORMAT")?;
    }
    let action = p.tool.rsplit('.').next().ok_or("DATABASE_TOOL")?;
    let value = remote(
        &p.scope,
        &p.profile,
        action,
        &input,
        matches!(action, "execute" | "import"),
    )
    .await?;
    finish(&p, value)
}

#[cfg(all(test, windows))]
#[path = "database_tests.rs"]
mod tests;
