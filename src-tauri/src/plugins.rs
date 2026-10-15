//! Reviewed, disabled-by-default local tools. Script bytes are untrusted code.
use crate::{policy, process, runtime::Core, storage};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) const TOOLS: &[&str] = &[
    "custom.list",
    "custom.inspect",
    "custom.register",
    "custom.run",
    "custom.configure",
    "custom.versions",
    "custom.update",
    "custom.rollback",
    "custom.uninstall",
    "plugin_builder.create",
    "plugin_builder.update",
    "plugin_builder.guide",
    "plugin_builder.test",
    "plugin_builder.enable_plugin",
];
fn guide() -> Value {
    json!({
        "workflow":["custom.list / inspect first", "create(manifest,files) or update(id,expected,manifest,files)", "inspect for current fingerprint", "test(id,expected)", "inspect again, then enable_plugin(id,expected)"],
        "manifest_example":{"id":"custom.example","name":"Example","version":"1.0.0","type":"custom","runtime":"node","entrypoint":"index.mjs","test":"test.mjs","description":"Describe the tool","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false},"outputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false},"permissions":{"defaultRisk":"mandatory","network":false,"filesystem":[]},"timeoutMs":5000,"sideEffects":["Declare actual effects"],"rollback":"Describe rollback and limits","secrets":[],"fixtures":{"happy":{"text":"hello"},"badInput":{}}},
        "files":"Object mapping simple relative file names to UTF-8 content; include entrypoint, distinct test script, README.md and icon.svg. Maximum 16 files / 1 MiB total. No paths or embedded credentials.",
        "runner":"Entrypoint reads a JSON object from stdin and emits only JSON matching outputSchema on stdout. Test script: ALFREDPET_TEST_CASE=contract must assert success and invalid input; =timeout must remain running more than one second. Test suite additionally checks permission denial, output schema and redaction. Scripts run with user OS privileges and filtered environment, not a guaranteed OS sandbox.",
        "updates":"Use a higher X.Y.Z version with the same id. Builder writes a new managed directory, preserves the prior version and publishes disabled. Retest and enable separately, with human permission for each mutation. Never write source in place.",
        "secrets":"Configure ALFREDPET_SECRET_* references privately in Settings, never include secret values in generated code or chat."
    })
}
pub(crate) fn schema(tool: &str) -> Value {
    let id = json!({"type":"string","pattern":"^custom\\.[A-Za-z0-9][A-Za-z0-9._-]{0,100}$"});
    let expected = json!({"type":"object","properties":{"fingerprint":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"required":["fingerprint"],"additionalProperties":false});
    let (required, props) = match tool {
        "custom.list" | "plugin_builder.guide" => (vec![], json!({})),
        "custom.inspect" | "custom.versions" => (vec!["id"], json!({"id":id})),
        "custom.register" => (
            vec!["manifest", "path"],
            json!({"manifest":{"type":"object"},"path":{"type":"string","minLength":1}}),
        ),
        "plugin_builder.create" => (
            vec!["manifest", "files"],
            json!({"manifest":{"type":"object"},"files":{"type":"object","minProperties":1,"maxProperties":16,"additionalProperties":{"type":"string","maxLength":262144}}}),
        ),
        "plugin_builder.update" => (
            vec!["id", "expected", "manifest", "files"],
            json!({"id":id,"expected":expected,"manifest":{"type":"object"},"files":{"type":"object","minProperties":1,"maxProperties":16,"additionalProperties":{"type":"string","maxLength":262144}}}),
        ),
        "custom.update" => (
            vec!["id", "expected", "manifest", "path"],
            json!({"id":id,"expected":expected,"manifest":{"type":"object"},"path":{"type":"string","minLength":1}}),
        ),
        "custom.run" => (
            vec!["id", "expected", "body"],
            json!({"id":id,"expected":expected,"body":{"type":"object"}}),
        ),
        "custom.configure" => (
            vec!["id", "expected", "enabled"],
            json!({"id":id,"expected":expected,"enabled":{"const":false}}),
        ),
        "custom.rollback" => (
            vec!["id", "expected", "version"],
            json!({"id":id,"expected":expected,"version":{"type":"string","maxLength":32}}),
        ),
        _ if TOOLS.contains(&tool) => {
            (vec!["id", "expected"], json!({"id":id,"expected":expected}))
        }
        _ => return json!({"not":{}}),
    };
    json!({"type":"object","required":required,"properties":props,"additionalProperties":false})
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Permissions {
    #[serde(rename = "defaultRisk")]
    risk: String,
    network: bool,
    filesystem: Vec<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fixtures {
    happy: Value,
    #[serde(rename = "badInput")]
    bad: Value,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    name: String,
    version: String,
    #[serde(rename = "type")]
    kind: String,
    runtime: String,
    entrypoint: String,
    test: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input: Value,
    #[serde(rename = "outputSchema")]
    output: Value,
    permissions: Permissions,
    #[serde(rename = "timeoutMs")]
    timeout: u64,
    #[serde(rename = "sideEffects")]
    effects: Vec<String>,
    rollback: String,
    #[serde(default)]
    secrets: Vec<String>,
    fixtures: Fixtures,
}
struct Offline;
impl jsonschema::Retrieve for Offline {
    fn retrieve(
        &self,
        _: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("PLUGIN_SCHEMA_EXTERNAL_REFERENCE".into())
    }
}
fn validator(v: &Value) -> Result<jsonschema::Validator, String> {
    bounded(v, 0)?;
    if v["type"] != "object" {
        return Err("PLUGIN_SCHEMA_OBJECT".into());
    }
    if let Some(s) = v.get("$schema") {
        if !s.as_str().is_some_and(|s| {
            [
                "https://json-schema.org/draft/2020-12/schema",
                "https://json-schema.org/draft/2019-09/schema",
                "http://json-schema.org/draft-07/schema#",
            ]
            .contains(&s)
        }) {
            return Err("PLUGIN_SCHEMA_DIALECT".into());
        }
    }
    jsonschema::options()
        .with_retriever(Offline)
        .with_pattern_options(
            jsonschema::PatternOptions::fancy_regex()
                .backtrack_limit(100_000)
                .size_limit(65536),
        )
        .build(v)
        .map_err(|_| "PLUGIN_SCHEMA_INVALID".into())
}
fn bounded(v: &Value, depth: usize) -> Result<(), String> {
    if depth > 16 || v.to_string().len() > 65536 {
        return Err("PLUGIN_VALUE_LIMIT".into());
    }
    match v {
        Value::Array(a) => {
            for v in a {
                bounded(v, depth + 1)?
            }
        }
        Value::Object(m) => {
            for v in m.values() {
                bounded(v, depth + 1)?
            }
        }
        _ => (),
    };
    Ok(())
}
fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !s.starts_with('.')
        && !s.contains("..")
}
fn id(s: &str) -> bool {
    s.strip_prefix("custom.").is_some_and(safe_name)
}
fn version(s: &str) -> Result<[u32; 3], String> {
    let p = s.split('.').collect::<Vec<_>>();
    if p.len() != 3 {
        return Err("PLUGIN_VERSION".into());
    }
    let mut out = [0; 3];
    for (i, v) in p.iter().enumerate() {
        out[i] = v.parse().map_err(|_| "PLUGIN_VERSION")?;
        if out[i].to_string() != *v {
            return Err("PLUGIN_VERSION".into());
        }
    }
    Ok(out)
}
fn manifest(v: &Value) -> Result<Manifest, String> {
    bounded(v, 0)?;
    let m: Manifest = serde_json::from_value(v.clone()).map_err(|_| "PLUGIN_MANIFEST")?;
    if !id(&m.id)
        || m.kind != "custom"
        || m.name.trim().is_empty()
        || m.name.len() > 200
        || m.description.len() > 4000
        || !safe_name(&m.entrypoint)
        || !safe_name(&m.test)
        || m.entrypoint == m.test
        || !["node", "python", "powershell", "executable"].contains(&m.runtime.as_str())
    {
        return Err("PLUGIN_MANIFEST".into());
    }
    version(&m.version)?;
    if m.permissions.risk != "mandatory"
        || m.permissions.filesystem.len() > 16
        || !(100..=60000).contains(&m.timeout)
        || m.effects.is_empty()
        || m.effects.len() > 16
        || m.effects
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 2000)
        || m.rollback.trim().is_empty()
        || m.rollback.len() > 4000
    {
        return Err("PLUGIN_REVIEW_REQUIRED".into());
    }
    if m.secrets.len() > 8
        || m.secrets.iter().any(|s| {
            !s.starts_with("ALFREDPET_SECRET_")
                || s.len() > 64
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        })
        || m.secrets
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != m.secrets.len()
    {
        return Err("PLUGIN_SECRET_REFERENCES".into());
    }
    // Schemas and explicit reference names may describe sensitive fields, but
    // executable declarations never contain recognizable literal credentials.
    for s in [&m.name, &m.description, &m.rollback]
        .into_iter()
        .chain(m.effects.iter())
    {
        if ["sk-ant-", "ghp_", "github_pat_", "-----BEGIN"]
            .iter()
            .any(|p| s.contains(p))
        {
            return Err("PLUGIN_HIDDEN_SECRET".into());
        }
    }
    let input = validator(&m.input)?;
    validator(&m.output)?;
    bounded(&m.fixtures.happy, 0)?;
    bounded(&m.fixtures.bad, 0)?;
    if !input.is_valid(&m.fixtures.happy) || input.is_valid(&m.fixtures.bad) {
        return Err("PLUGIN_TEST_FIXTURES".into());
    }
    Ok(m)
}
fn row(c: &Core, id: &str) -> Result<Value, String> {
    if !self::id(id) {
        return Err("PLUGIN_ID".into());
    }
    let rows = c
        .store
        .query("SELECT * FROM custom_tools WHERE id=?", &[json!(id)])?;
    if rows[0].is_null() {
        return Err("PLUGIN_NOT_FOUND".into());
    }
    Ok(rows[0].clone())
}
fn fingerprint(c: &Core, row: &Value) -> Result<String, String> {
    let raw = row["install_path"].as_str().ok_or("PLUGIN_PATH")?;
    let path = c.path(raw, false)?;
    if !path.is_dir() {
        return Err("PLUGIN_PATH".into());
    }
    let mut entries = std::fs::read_dir(&path)
        .map_err(|_| "PLUGIN_READ")?
        .map(|r| r.map(|r| r.path()).map_err(|_| "PLUGIN_READ"))
        .collect::<Result<Vec<_>, _>>()?;
    if entries.is_empty() || entries.len() > 64 {
        return Err("PLUGIN_FILE_LIMIT".into());
    }
    entries.sort();
    let mut hash = Sha256::new();
    hash.update(row.to_string().as_bytes());
    let mut total = 0;
    for raw in entries {
        let p = c.path(raw.to_str().ok_or("PLUGIN_PATH")?, false)?;
        if !p.is_file() || p.parent() != Some(path.as_path()) {
            return Err("PLUGIN_FLAT_DIRECTORY_REQUIRED".into());
        }
        let bytes = crate::modules::bounded_read(&p)?;
        total += bytes.len();
        if bytes.len() > 4 * 1024 * 1024 || total > 16 * 1024 * 1024 {
            return Err("PLUGIN_FILE_LIMIT".into());
        }
        if p.extension().is_some_and(|e| {
            ["mjs", "js", "py", "ps1", "json", "md"].contains(&e.to_str().unwrap_or(""))
        }) && String::from_utf8_lossy(&bytes).lines().any(|l| {
            policy::contains_secret(l)
                && ["sk-ant-", "ghp_", "github_pat_", "-----BEGIN"]
                    .iter()
                    .any(|s| l.contains(s))
        }) {
            return Err("PLUGIN_HIDDEN_SECRET".into());
        }
        hash.update(
            p.file_name()
                .ok_or("PLUGIN_PATH")?
                .to_string_lossy()
                .as_bytes(),
        );
        hash.update(Sha256::digest(&bytes));
    }
    hash.update(
        c.store
            .setting(&format!(
                "plugin_secrets.{}",
                row["id"].as_str().ok_or("PLUGIN_ID")?
            ))
            .to_string()
            .as_bytes(),
    );
    Ok(format!("{:x}", hash.finalize()))
}
fn observed_fingerprint(c: &Core, row: &Value) -> (String, Option<String>) {
    match fingerprint(c, row) {
        Ok(fp) => (fp, None),
        Err(error) => (
            format!(
                "{:x}",
                Sha256::digest(
                    json!({"row":row,"unavailable":error})
                        .to_string()
                        .as_bytes()
                )
            ),
            Some(error),
        ),
    }
}
fn passive(tool: &str) -> bool {
    [
        "custom.inspect",
        "custom.versions",
        "custom.configure",
        "custom.uninstall",
    ]
    .contains(&tool)
}
fn runtime(c: &Core, m: &Manifest, path: &Path) -> Result<PathBuf, String> {
    let binary = match m.runtime.as_str() {
        "node" if c.resource.join("runtime/node.exe").is_file() => {
            c.resource.join("runtime/node.exe")
        }
        "node" => find_binary("node.exe")?,
        "python" => find_binary("python.exe")?,
        "powershell" => {
            PathBuf::from(std::env::var_os("SystemRoot").ok_or("PLUGIN_RUNTIME_MISSING")?)
                .join("System32/WindowsPowerShell/v1.0/powershell.exe")
        }
        "executable" => path.join(&m.entrypoint),
        _ => return Err("PLUGIN_RUNTIME_UNSUPPORTED".into()),
    };
    if !binary.is_file() {
        return Err("PLUGIN_RUNTIME_MISSING".into());
    }
    binary
        .canonicalize()
        .map_err(|_| "PLUGIN_RUNTIME_MISSING".into())
}
fn find_binary(name: &str) -> Result<PathBuf, String> {
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        if dir.is_absolute() && dir.join(name).is_file() {
            return Ok(dir.join(name));
        }
    }
    Err("PLUGIN_RUNTIME_MISSING".into())
}
fn validate_path(c: &Core, m: &Manifest, raw: &str) -> Result<PathBuf, String> {
    let p = c.path(raw, false)?;
    if !p.is_dir() {
        return Err("PLUGIN_PATH".into());
    }
    for entry in [&m.entrypoint, &m.test] {
        let e = c.path(p.join(entry).to_str().ok_or("PLUGIN_PATH")?, false)?;
        if !e.is_file() || e.parent() != Some(p.as_path()) {
            return Err("PLUGIN_ENTRY".into());
        }
    }
    for root in &m.permissions.filesystem {
        c.path(root, false)?;
    }
    runtime(c, m, &p)?;
    Ok(p)
}
fn authority(c: &Core) -> Result<String, String> {
    let rows = c.store.query(
        "SELECT json_value FROM settings WHERE key='plugin_authority'",
        &[],
    )?;
    let v: Value = serde_json::from_str(rows[0]["json_value"].as_str().ok_or("PLUGIN_AUTHORITY")?)
        .map_err(|_| "PLUGIN_AUTHORITY")?;
    let id = v.as_str().ok_or("PLUGIN_AUTHORITY")?;
    if uuid::Uuid::parse_str(id).is_err() {
        return Err("PLUGIN_AUTHORITY".into());
    }
    Ok(id.into())
}
pub(crate) fn revoke(c: &Core) -> Result<(), String> {
    c.store
        .set_setting("plugin_authority", &json!(storage::id()))
}
pub(crate) struct Prepared {
    tool: String,
    input: Value,
    authority: String,
    original: Option<Value>,
    fingerprint: Option<String>,
    manifest: Option<Manifest>,
    path: Option<PathBuf>,
    binary: Option<PathBuf>,
    candidate: Option<String>,
}
impl Prepared {
    pub(crate) fn prepare(c: &Core, tool: &str, v: &Value) -> Result<Self, String> {
        c.store.execute(
            "INSERT OR IGNORE INTO settings(key,json_value) VALUES('plugin_authority',?)",
            &[json!(json!(storage::id()).to_string())],
        )?;
        let original = if v["id"].is_string() {
            Some(row(c, v["id"].as_str().unwrap())?)
        } else {
            None
        };
        let fp = original
            .as_ref()
            .map(|r| {
                if passive(tool) {
                    Ok(observed_fingerprint(c, r).0)
                } else {
                    fingerprint(c, r)
                }
            })
            .transpose()?;
        if let Some(fp) = &fp {
            if !["custom.inspect", "custom.versions"].contains(&tool)
                && v["expected"]["fingerprint"] != *fp
            {
                return Err("PLUGIN_CHANGED".into());
            }
        }
        let mut m = None;
        let mut path = None;
        let mut binary = None;
        if let Some(manifest_input) = v.get("manifest") {
            let parsed = manifest(manifest_input)?;
            if let Some(old) = &original {
                let oldm: Value =
                    serde_json::from_str(old["manifest_json"].as_str().ok_or("PLUGIN_MANIFEST")?)
                        .map_err(|_| "PLUGIN_MANIFEST")?;
                if parsed.id != old["id"]
                    || version(&parsed.version)?
                        <= version(oldm["version"].as_str().ok_or("PLUGIN_VERSION")?)?
                {
                    return Err("PLUGIN_NEW_VERSION_REQUIRED".into());
                }
            } else if !c.store.query(
                "SELECT id FROM custom_tools WHERE id=?",
                &[json!(parsed.id)],
            )?[0]
                .is_null()
            {
                return Err("PLUGIN_ID_EXISTS".into());
            }
            if ["plugin_builder.create", "plugin_builder.update"].contains(&tool) {
                validate_files(&parsed, &v["files"])?;
            } else {
                let p = validate_path(c, &parsed, v["path"].as_str().ok_or("PLUGIN_PATH")?)?;
                binary = Some(runtime(c, &parsed, &p)?);
                path = Some(p);
            }
            m = Some(parsed);
        } else if let Some(old) = &original {
            if [
                "custom.run",
                "plugin_builder.test",
                "plugin_builder.enable_plugin",
            ]
            .contains(&tool)
            {
                let parsed = manifest(
                    &serde_json::from_str(old["manifest_json"].as_str().ok_or("PLUGIN_MANIFEST")?)
                        .map_err(|_| "PLUGIN_MANIFEST")?,
                )?;
                let p = validate_path(
                    c,
                    &parsed,
                    old["install_path"].as_str().ok_or("PLUGIN_PATH")?,
                )?;
                binary = Some(runtime(c, &parsed, &p)?);
                path = Some(p);
                if tool == "custom.run" {
                    if old["state"] != "enabled" {
                        return Err("PLUGIN_DISABLED".into());
                    }
                    bounded(&v["body"], 0)?;
                    if !validator(&parsed.input)?.is_valid(&v["body"]) {
                        return Err("PLUGIN_INPUT_INVALID".into());
                    }
                }
                if ["custom.run", "plugin_builder.enable_plugin"].contains(&tool) {
                    let tested = c.store.setting(&format!("plugin_test.{}", parsed.id));
                    if tested["code_fingerprint"] != code_fingerprint(c, old)?
                        || tested["status"] != "PASS"
                    {
                        return Err("PLUGIN_CHANGED_RETEST_REQUIRED".into());
                    }
                }
                m = Some(parsed);
            }
        }
        let candidate = if let (Some(m), Some(p)) = (&m, &path) {
            Some(fingerprint(
                c,
                &json!({"id":m.id,"manifest_json":json!(m).to_string(),"install_path":p,"state":"candidate"}),
            )?)
        } else {
            None
        };
        let p = Self {
            tool: tool.into(),
            input: v.clone(),
            authority: authority(c)?,
            original,
            fingerprint: fp,
            manifest: m,
            path,
            binary,
            candidate,
        };
        p.check(c)?;
        Ok(p)
    }
    pub(crate) fn review(&self) -> Value {
        json!({"tool":self.tool,"id":self.input["id"],"fingerprint":self.fingerprint,"manifest":self.manifest.as_ref().map(|m|json!(m)),"working_directory":self.path.as_ref().map(|p|json!(p)).or_else(||self.original.as_ref().map(|r|r["install_path"].clone())),"runtime_executable":self.binary,"input":self.input,"isolation":"Owned process tree, filtered environment and bounds. Local scripts can access the current user's OS resources; declared network/filesystem permissions do not enforce an OS sandbox.","auto_enable":false})
    }
    fn scope(&self, c: &Core) -> Result<(), String> {
        if c.store.setting("privacy") == true
            || !c.enabled(self.tool.split('.').next().ok_or("PLUGIN_TOOL")?)
            || authority(c)? != self.authority
        {
            return Err("PLUGIN_REVOKED".into());
        }
        Ok(())
    }
    pub(crate) fn check(&self, c: &Core) -> Result<(), String> {
        self.scope(c)?;
        if let Some(old) = &self.original {
            if row(c, old["id"].as_str().ok_or("PLUGIN_ID")?)? != *old
                || (if passive(&self.tool) {
                    observed_fingerprint(c, old).0
                } else {
                    fingerprint(c, old)?
                }) != *self.fingerprint.as_ref().ok_or("PLUGIN_FINGERPRINT")?
            {
                return Err("PLUGIN_CHANGED".into());
            }
        }
        if ["custom.register", "custom.update"].contains(&self.tool.as_str()) {
            let m = self.manifest.as_ref().ok_or("PLUGIN_MANIFEST")?;
            let p = validate_path(c, m, self.input["path"].as_str().ok_or("PLUGIN_PATH")?)?;
            if Some(&p) != self.path.as_ref()
                || Some(runtime(c, m, &p)?) != self.binary
                || Some(fingerprint(
                    c,
                    &json!({"id":m.id,"manifest_json":json!(m).to_string(),"install_path":p,"state":"candidate"}),
                )?) != self.candidate
            {
                return Err("PLUGIN_CHANGED".into());
            }
        }
        Ok(())
    }
    async fn guard<T>(
        &self,
        c: &Core,
        f: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        tokio::pin!(f);
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            tokio::select! {result=&mut f=>{self.check(c)?;return result},_=interval.tick()=>self.check(c)?}
        }
    }
    pub(crate) async fn execute(self, c: &Arc<Core>) -> Result<Value, String> {
        let _lease = c.plugin_actions.try_lock().map_err(|_| "PLUGIN_BUSY")?;
        self.check(c)?;
        match self.tool.as_str() {
            "custom.list" => list(c),
            "custom.inspect" => inspect(c, self.input["id"].as_str().ok_or("PLUGIN_ID")?),
            "custom.versions" => Ok(c.store.setting(&format!(
                "plugin_versions.{}",
                self.input["id"].as_str().ok_or("PLUGIN_ID")?
            ))),
            "custom.run" => {
                let m = self.manifest.as_ref().ok_or("PLUGIN_MANIFEST")?;
                let secrets =
                    crate::plugin_secrets::load(c, m.secrets.as_slice(), &m.id, &m.version).await?;
                self.check(c)?;
                let output = self
                    .guard(
                        c,
                        run(
                            self.binary.as_ref().ok_or("PLUGIN_RUNTIME")?,
                            m,
                            self.path.as_ref().ok_or("PLUGIN_PATH")?,
                            None,
                            &self.input["body"],
                            &secrets,
                            Some(Permit),
                        ),
                    )
                    .await
                    .map_err(|e| format!("PLUGIN_OUTCOME_UNCERTAIN:{e}"))?;
                let value: Value = serde_json::from_str(&output)
                    .map_err(|_| "PLUGIN_OUTCOME_UNCERTAIN:PLUGIN_OUTPUT_JSON")?;
                bounded(&value, 0)?;
                if !validator(&m.output)?.is_valid(&value) {
                    return Err("PLUGIN_OUTCOME_UNCERTAIN:PLUGIN_OUTPUT_SCHEMA".into());
                }
                Ok(redact(&value, &secrets))
            }
            "plugin_builder.test" => self.test(c).await,
            "plugin_builder.guide" => Ok(guide()),
            "plugin_builder.create" | "plugin_builder.update" => {
                let m = self.manifest.as_ref().ok_or("PLUGIN_MANIFEST")?;
                let base = c.root.join("plugins");
                std::fs::create_dir_all(&base).map_err(|_| "PLUGIN_DIRECTORY")?;
                let path = base.join(format!("{}-{}-{}", m.id, m.version, storage::id()));
                std::fs::create_dir(&path).map_err(|_| "PLUGIN_DIRECTORY")?;
                for (name, value) in self.input["files"].as_object().ok_or("PLUGIN_FILES")? {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(path.join(name))
                        .map_err(|_| "PLUGIN_WRITE")?;
                    f.write_all(value.as_str().ok_or("PLUGIN_FILE")?.as_bytes())
                        .map_err(|_| "PLUGIN_WRITE")?;
                }
                std::fs::write(
                    path.join("manifest.json"),
                    serde_json::to_vec_pretty(m).map_err(|_| "PLUGIN_MANIFEST")?,
                )
                .map_err(|_| "PLUGIN_WRITE")?;
                self.check(c)?;
                validate_path(c, m, path.to_str().ok_or("PLUGIN_PATH")?)?;
                if self.tool == "plugin_builder.update" {
                    return self.update_at(c, &path);
                }
                c.store
                    .execute(
                        "INSERT INTO custom_tools VALUES(?,?,?,'draft')",
                        &[json!(m.id), json!(json!(m).to_string()), json!(path)],
                    )
                    .map_err(|_| "PLUGIN_REGISTER_FAILED")?;
                Ok(json!({"id":m.id,"path":path,"state":"draft","enabled":false}))
            }
            "custom.register" => {
                let m = self.manifest.as_ref().ok_or("PLUGIN_MANIFEST")?;
                c.store
                    .execute(
                        "INSERT INTO custom_tools VALUES(?,?,?,'validated')",
                        &[json!(m.id), json!(json!(m).to_string()), json!(self.path)],
                    )
                    .map_err(|_| "PLUGIN_REGISTER_FAILED")?;
                Ok(json!({"id":m.id,"state":"validated","enabled":false}))
            }
            "plugin_builder.enable_plugin" => {
                let old = self.original.as_ref().ok_or("PLUGIN_NOT_FOUND")?;
                if !["tested", "disabled"].iter().any(|s| old["state"] == *s) {
                    return Err("PLUGIN_TEST_REQUIRED".into());
                }
                cas(c, old, "enabled")?;
                Ok(json!({"enabled":true}))
            }
            "custom.configure" => {
                cas(
                    c,
                    self.original.as_ref().ok_or("PLUGIN_NOT_FOUND")?,
                    "disabled",
                )?;
                Ok(json!({"enabled":false}))
            }
            "custom.update" => self.update(c),
            "custom.rollback" => self.rollback(c),
            "custom.uninstall" => {
                let old = self.original.as_ref().ok_or("PLUGIN_NOT_FOUND")?;
                if c.store.execute("DELETE FROM custom_tools WHERE id=? AND manifest_json=? AND install_path=? AND state=?",&[old["id"].clone(),old["manifest_json"].clone(),old["install_path"].clone(),old["state"].clone()])?!=1{return Err("PLUGIN_CHANGED".into())}
                Ok(json!({"unregistered":true,"files_preserved":true}))
            }
            _ => Err("PLUGIN_TOOL".into()),
        }
    }
    async fn test(&self, c: &Core) -> Result<Value, String> {
        let m = self.manifest.as_ref().ok_or("PLUGIN_MANIFEST")?;
        let bin = self.binary.as_ref().ok_or("PLUGIN_RUNTIME")?;
        let p = self.path.as_ref().ok_or("PLUGIN_PATH")?;
        let fake = m
            .secrets
            .iter()
            .map(|k| (k.clone(), "owned-fixture-private-value".into()))
            .collect::<BTreeMap<String, String>>();
        let mut results = Vec::new();
        let happy = self
            .guard(
                c,
                run(bin, m, p, None, &m.fixtures.happy, &fake, Some(Permit)),
            )
            .await;
        let happy = happy
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
        let command = self
            .guard(
                c,
                run(
                    bin,
                    m,
                    p,
                    Some("contract"),
                    &m.fixtures.happy,
                    &fake,
                    Some(Permit),
                ),
            )
            .await;
        results.push(json!({"case":"happy_path","passed":happy.is_some()&&command.is_ok()}));
        let bad = run(bin, m, p, None, &m.fixtures.bad, &fake, Some(Permit)).await;
        results
            .push(json!({"case":"bad_input","passed":bad.as_ref().is_err_and(|e|e=="PLUGIN_INPUT_INVALID")}));
        results.push(json!({"case":"output_schema","passed":happy.as_ref().is_some_and(|v|validator(&m.output).is_ok_and(|s|s.is_valid(v)))}));
        let denied = run(bin, m, p, None, &m.fixtures.happy, &fake, None).await;
        results.push(json!({"case":"denied_permission","passed":denied.as_ref().is_err_and(|e|e=="PLUGIN_PERMISSION_REQUIRED")}));
        let timeout = self
            .guard(
                c,
                run(
                    bin,
                    m,
                    p,
                    Some("timeout"),
                    &m.fixtures.happy,
                    &fake,
                    Some(Permit),
                ),
            )
            .await;
        results.push(
            json!({"case":"timeout","passed":timeout.as_ref().is_err_and(|e|e=="PLUGIN_TEST_TIMEOUT")}),
        );
        let cleaned = redact(
            &json!({"value":"owned-fixture-private-value","password":"fixture","authorization":"Bearer fixture"}),
            &BTreeMap::from([(
                "ALFREDPET_SECRET_FIXTURE".into(),
                "owned-fixture-private-value".into(),
            )]),
        );
        results.push(json!({"case":"secret_redaction","passed":!cleaned.to_string().contains("owned-fixture-private-value")&&!cleaned.to_string().contains("Bearer fixture")}));
        self.check(c)?;
        let passed = results.iter().all(|v| v["passed"] == true);
        let old = self.original.as_ref().ok_or("PLUGIN_NOT_FOUND")?;
        let fp = code_fingerprint(c, old)?;
        c.store.set_setting(&format!("plugin_test.{}",m.id),&json!({"status":if passed{"PASS"}else{"FAIL"},"cases":results,"at":storage::now(),"code_fingerprint":fp}))?;
        self.check(c)?;
        cas(c, old, if passed { "tested" } else { "quarantined" })?;
        Ok(
            json!({"status":if passed{"PASS"}else{"FAIL"},"cases":results,"state":if passed{"tested"}else{"quarantined"},"enabled":false}),
        )
    }
    fn update(&self, c: &Core) -> Result<Value, String> {
        self.update_at(c, self.path.as_ref().ok_or("PLUGIN_PATH")?)
    }
    fn update_at(&self, c: &Core, path: &Path) -> Result<Value, String> {
        let old = self.original.as_ref().ok_or("PLUGIN_NOT_FOUND")?;
        let m = self.manifest.as_ref().ok_or("PLUGIN_MANIFEST")?;
        if old["install_path"] == json!(path) {
            return Err("PLUGIN_SEPARATE_VERSION_DIRECTORY_REQUIRED".into());
        }
        let mut history = c.store.setting(&format!("plugin_versions.{}", m.id));
        if history.is_null() {
            history = json!([])
        }
        let entries = history.as_array_mut().ok_or("PLUGIN_VERSION_HISTORY")?;
        if entries.len() >= 10 {
            return Err("PLUGIN_VERSION_HISTORY_LIMIT".into());
        }
        let mut prior = old.clone();
        prior["code_fingerprint"] = json!(code_fingerprint(c, old)?);
        entries.push(prior);
        publish_version(c, old, &json!(m).to_string(), path, &history)?;
        Ok(
            json!({"id":m.id,"version":m.version,"state":"validated","enabled":false,"previous_preserved":true,"migration":"No automatic migration script is executed; use a separately reviewed tool if needed."}),
        )
    }
    fn rollback(&self, c: &Core) -> Result<Value, String> {
        let old = self.original.as_ref().ok_or("PLUGIN_NOT_FOUND")?;
        let id = old["id"].as_str().ok_or("PLUGIN_ID")?;
        let history = c.store.setting(&format!("plugin_versions.{id}"));
        let entries = history.as_array().ok_or("PLUGIN_VERSION_HISTORY")?;
        let previous = entries
            .iter()
            .rev()
            .find(|r| {
                serde_json::from_str::<Value>(r["manifest_json"].as_str().unwrap_or(""))
                    .is_ok_and(|m| m["version"] == self.input["version"])
            })
            .ok_or("PLUGIN_VERSION_NOT_FOUND")?;
        let mut row = previous.clone();
        let expected = row
            .as_object_mut()
            .ok_or("PLUGIN_VERSION_HISTORY")?
            .remove("code_fingerprint")
            .ok_or("PLUGIN_VERSION_HISTORY")?;
        if code_fingerprint(c, &row)? != expected {
            return Err("PLUGIN_PREVIOUS_VERSION_CHANGED".into());
        }
        let path = c.path(row["install_path"].as_str().ok_or("PLUGIN_PATH")?, false)?;
        publish_version(
            c,
            old,
            row["manifest_json"].as_str().ok_or("PLUGIN_MANIFEST")?,
            &path,
            &history,
        )?;
        Ok(
            json!({"version":self.input["version"],"state":"validated","enabled":false,"retest_required":true}),
        )
    }
}
fn validate_files(m: &Manifest, v: &Value) -> Result<(), String> {
    let files = v.as_object().ok_or("PLUGIN_FILES")?;
    if files.is_empty()
        || files.len() > 16
        || !files.contains_key(&m.entrypoint)
        || !files.contains_key(&m.test)
        || !files.contains_key("README.md")
        || !files.contains_key("icon.svg")
    {
        return Err("PLUGIN_FILES_REQUIRED".into());
    }
    let mut total = 0;
    for (name, v) in files {
        let s = v.as_str().ok_or("PLUGIN_FILE")?;
        total += s.len();
        if !safe_name(name) || name == "manifest.json" || s.len() > 262144 || total > 1048576 {
            return Err("PLUGIN_FILE_LIMIT".into());
        }
        if ["sk-ant-", "ghp_", "github_pat_", "-----BEGIN"]
            .iter()
            .any(|p| s.contains(p))
        {
            return Err("PLUGIN_HIDDEN_SECRET".into());
        }
    }
    Ok(())
}
fn code_fingerprint(c: &Core, r: &Value) -> Result<String, String> {
    let mut row = r.clone();
    row["state"] = json!("code");
    fingerprint(c, &row)
}
fn cas(c: &Core, old: &Value, state: &str) -> Result<(), String> {
    if c.store.execute("UPDATE custom_tools SET state=? WHERE id=? AND manifest_json=? AND install_path=? AND state=?",&[json!(state),old["id"].clone(),old["manifest_json"].clone(),old["install_path"].clone(),old["state"].clone()])?!=1{return Err("PLUGIN_CHANGED".into())}
    Ok(())
}
fn publish_version(
    c: &Core,
    old: &Value,
    manifest: &str,
    path: &Path,
    history: &Value,
) -> Result<(), String> {
    let mut db = c.store.0.lock().map_err(|_| "PLUGIN_STORE")?;
    let tx = db.transaction().map_err(|_| "PLUGIN_STORE")?;
    if tx.execute("UPDATE custom_tools SET manifest_json=?,install_path=?,state='validated' WHERE id=? AND manifest_json=? AND install_path=? AND state=?",rusqlite::params![manifest,path.to_string_lossy(),old["id"].as_str(),old["manifest_json"].as_str(),old["install_path"].as_str(),old["state"].as_str()]).map_err(|_|"PLUGIN_STORE")?!=1{return Err("PLUGIN_CHANGED".into())}
    tx.execute("INSERT INTO settings(key,json_value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET json_value=excluded.json_value",rusqlite::params![format!("plugin_versions.{}",old["id"].as_str().ok_or("PLUGIN_ID")?),history.to_string()]).map_err(|_|"PLUGIN_STORE")?;
    tx.commit()
        .map_err(|_| "PLUGIN_OUTCOME_UNCERTAIN:STORE_COMMIT".into())
}
fn list(c: &Core) -> Result<Value, String> {
    let rows = c.store.query(
        "SELECT id,manifest_json,state FROM custom_tools ORDER BY id LIMIT 250",
        &[],
    )?;
    let mut out = Vec::new();
    for r in rows.as_array().ok_or("PLUGIN_ROWS")? {
        let m: Value = serde_json::from_str(r["manifest_json"].as_str().ok_or("PLUGIN_MANIFEST")?)
            .map_err(|_| "PLUGIN_MANIFEST")?;
        out.push(json!({"id":r["id"],"name":m["name"],"version":m["version"],"state":r["state"],"runtime":m["runtime"]}))
    }
    Ok(json!(out))
}
pub(crate) fn inspect(c: &Core, id: &str) -> Result<Value, String> {
    let r = row(c, id)?;
    let m: Value = serde_json::from_str(r["manifest_json"].as_str().ok_or("PLUGIN_MANIFEST")?)
        .map_err(|_| "PLUGIN_MANIFEST")?;
    let (fingerprint, error) = observed_fingerprint(c, &r);
    Ok(
        json!({"id":id,"manifest":m,"path":r["install_path"],"state":r["state"],"fingerprint":fingerprint,"health":if error.is_some(){"ERROR"}else{"OBSERVED"},"error_code":error,"tests":c.store.setting(&format!("plugin_test.{id}")),"isolation":"Process tree and filtered environment; OS filesystem/network resources remain accessible to reviewed script code."}),
    )
}
struct Permit;
async fn run(
    binary: &Path,
    m: &Manifest,
    path: &Path,
    test: Option<&str>,
    input: &Value,
    secrets: &BTreeMap<String, String>,
    permit: Option<Permit>,
) -> Result<String, String> {
    if permit.is_none() {
        return Err("PLUGIN_PERMISSION_REQUIRED".into());
    }
    if !validator(&m.input)?.is_valid(input) {
        return Err("PLUGIN_INPUT_INVALID".into());
    }
    let test_binary = path.join(&m.test);
    let mut cmd = tokio::process::Command::new(if m.runtime == "executable" && test.is_some() {
        test_binary.as_path()
    } else {
        binary
    });
    cmd.current_dir(path).env_clear();
    for key in ["PATH", "SystemRoot", "TEMP", "TMP"] {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    for (key, value) in secrets {
        cmd.env(key, value);
    }
    if m.runtime == "powershell" {
        cmd.args(["-NoProfile", "-NonInteractive", "-File"]);
    }
    if m.runtime != "executable" {
        cmd.arg(path.join(if test.is_some() {
            &m.test
        } else {
            &m.entrypoint
        }));
    }
    if let Some(mode) = test {
        cmd.env("ALFREDPET_TEST_CASE", mode);
    }
    let ms = if test == Some("timeout") {
        1000
    } else {
        m.timeout
    };
    let timeout = tokio::time::timeout(
        std::time::Duration::from_millis(ms),
        process::output(cmd, Some(&input.to_string()), ms.div_ceil(1000) + 1),
    )
    .await;
    let out = timeout.map_err(|_| {
        if test == Some("timeout") {
            "PLUGIN_TEST_TIMEOUT"
        } else {
            "PLUGIN_TIMEOUT"
        }
    })??;
    if out.len() > 65536 {
        return Err("PLUGIN_OUTPUT_LIMIT".into());
    }
    Ok(out)
}
fn redact(v: &Value, secrets: &BTreeMap<String, String>) -> Value {
    fn clean(v: &Value, secrets: &BTreeMap<String, String>) -> Value {
        match v {
            Value::String(s) => {
                let mut text = s.clone();
                for secret in secrets.values() {
                    if !secret.is_empty() {
                        text = text.replace(secret, "[REDACTED]");
                    }
                }
                json!(text)
            }
            Value::Array(a) => json!(a.iter().map(|v| clean(v, secrets)).collect::<Vec<_>>()),
            Value::Object(m) => Value::Object(
                m.iter()
                    .map(|(k, v)| {
                        let mut key = k.clone();
                        for secret in secrets.values() {
                            if !secret.is_empty() {
                                key = key.replace(secret, "[REDACTED]");
                            }
                        }
                        (key, clean(v, secrets))
                    })
                    .collect(),
            ),
            _ => v.clone(),
        }
    }
    policy::redact(&clean(v, secrets))
}
#[cfg(test)]
#[path = "plugin_contracts.rs"]
mod tests;
