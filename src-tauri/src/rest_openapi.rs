//! OpenAPI JSON -> bounded local proposals. No network, profile writes, auth
//! grants, remote references or server expansion happen during conversion.
use crate::rest_profile::{self, Auth, Config, Operation, ResponseKind, ResponseSpec, Risk};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub path: String,
    pub method: String,
    pub code: String,
}
#[derive(Debug, Serialize)]
pub struct Preview {
    pub version: String,
    pub profile: Config,
    pub diagnostics: Vec<Diagnostic>,
    pub operations_seen: usize,
    pub operations_proposed: usize,
    pub automatically_enabled: bool,
}
struct Convert<'a> {
    doc: &'a Value,
    legacy: bool,
    nodes: usize,
}
#[derive(Clone, Copy, PartialEq)]
enum Context {
    Request,
    Response,
    Parameter,
}
fn fail(s: &str) -> String {
    s.into()
}
fn atom(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
impl Convert<'_> {
    fn tick(&mut self, depth: usize) -> Result<(), String> {
        self.nodes += 1;
        if depth > 24 || self.nodes > 32768 {
            return Err(fail("OPENAPI_EXPANSION_LIMIT"));
        }
        Ok(())
    }
    fn pointer(&self, s: &str) -> Result<&Value, String> {
        if !s.starts_with("#/") || s.len() > 1024 || s.contains('%') {
            return Err(fail("OPENAPI_LOCAL_POINTER_REQUIRED"));
        }
        self.doc
            .pointer(&s[1..])
            .ok_or(fail("OPENAPI_REFERENCE_MISSING"))
    }
    fn object(&mut self, v: &Value) -> Result<Value, String> {
        let mut value = v.clone();
        let mut seen = BTreeSet::new();
        for depth in 0..=24 {
            self.tick(depth)?;
            let m = value.as_object().ok_or(fail("OPENAPI_OBJECT_REQUIRED"))?;
            let Some(reference) = m.get("$ref") else {
                return Ok(value);
            };
            if m.keys()
                .any(|k| !matches!(k.as_str(), "$ref" | "summary" | "description"))
            {
                return Err(fail("OPENAPI_REFERENCE_SIBLING_UNSUPPORTED"));
            }
            let reference = reference.as_str().ok_or(fail("OPENAPI_REFERENCE_TYPE"))?;
            if !seen.insert(reference.to_string()) {
                return Err(fail("OPENAPI_REFERENCE_CYCLE"));
            }
            value = self.pointer(reference)?.clone();
        }
        Err(fail("OPENAPI_REFERENCE_DEPTH"))
    }
    fn annotations(
        &mut self,
        v: &Value,
        depth: usize,
        active: &mut BTreeSet<String>,
    ) -> Result<(bool, bool), String> {
        self.tick(depth)?;
        let Some(m) = v.as_object() else {
            return Ok((false, false));
        };
        let mut flags = (false, false);
        for (key, flag) in [("readOnly", &mut flags.0), ("writeOnly", &mut flags.1)] {
            if let Some(v) = m.get(key) {
                *flag = v
                    .as_bool()
                    .ok_or(fail("OPENAPI_READ_WRITE_ANNOTATION_TYPE"))?;
            }
        }
        if let Some(reference) = m.get("$ref") {
            let r = reference.as_str().ok_or(fail("OPENAPI_REFERENCE_TYPE"))?;
            if !active.insert(r.to_string()) {
                return Err(fail("OPENAPI_REFERENCE_CYCLE"));
            }
            let target = self.pointer(r)?.clone();
            let inherited = self.annotations(&target, depth + 1, active)?;
            active.remove(r);
            flags.0 |= inherited.0;
            flags.1 |= inherited.1;
        }
        if let Some(list) = m.get("allOf").and_then(Value::as_array) {
            for item in list {
                let inherited = self.annotations(item, depth + 1, active)?;
                flags.0 |= inherited.0;
                flags.1 |= inherited.1;
            }
        }
        if flags.0 && flags.1 {
            return Err(fail("OPENAPI_READ_WRITE_CONFLICT"));
        }
        Ok(flags)
    }
    fn schema(
        &mut self,
        v: &Value,
        depth: usize,
        active: &mut BTreeSet<String>,
        context: Context,
    ) -> Result<Value, String> {
        self.tick(depth)?;
        if v.is_boolean() && !self.legacy {
            return Ok(v.clone());
        }
        let m = v.as_object().ok_or(fail("OPENAPI_SCHEMA_OBJECT"))?;
        if let Some(reference) = m.get("$ref") {
            let reference = reference.as_str().ok_or(fail("OPENAPI_REFERENCE_TYPE"))?;
            if !active.insert(reference.to_string()) {
                return Err(fail("OPENAPI_REFERENCE_CYCLE"));
            }
            let target = self.pointer(reference)?.clone();
            let resolved = self.schema(&target, depth + 1, active, context)?;
            active.remove(reference);
            let siblings: Map<_, _> = m
                .iter()
                .filter(|(k, _)| {
                    !matches!(
                        k.as_str(),
                        "$ref" | "title" | "description" | "default" | "example" | "examples"
                    ) && !k.starts_with("x-")
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if siblings.is_empty() {
                return Ok(resolved);
            }
            if self.legacy {
                return Err(fail("OPENAPI_REFERENCE_SIBLING_UNSUPPORTED"));
            }
            let constraints = self.schema(&Value::Object(siblings), depth + 1, active, context)?;
            return Ok(json!({"allOf":[resolved,constraints]}));
        }
        let mut out = Map::new();
        let mut forbidden = BTreeSet::new();
        for (k, v) in m {
            if k.starts_with("x-")
                || matches!(
                    k.as_str(),
                    "title"
                        | "description"
                        | "default"
                        | "example"
                        | "examples"
                        | "deprecated"
                        | "xml"
                        | "externalDocs"
                        | "$defs"
                )
            {
                continue;
            }
            match k.as_str() {
                "$schema"
                    if !self.legacy
                        && matches!(
                            v.as_str(),
                            Some(
                                "https://json-schema.org/draft/2020-12/schema"
                                    | "https://spec.openapis.org/oas/3.1/dialect/base"
                                    | "https://spec.openapis.org/oas/3.2/dialect/base"
                            )
                        ) => {}
                "properties" | "patternProperties" | "dependentSchemas" => {
                    let props = v.as_object().ok_or(fail("OPENAPI_SCHEMA_PROPERTIES"))?;
                    let mut converted = Map::new();
                    for (name, s) in props {
                        let (read, write) = self.annotations(s, depth + 1, &mut BTreeSet::new())?;
                        let converted_schema = self.schema(s, depth + 1, active, context)?;
                        if k == "properties"
                            && ((read && context == Context::Request)
                                || (write && context == Context::Response))
                        {
                            forbidden.insert(name.clone());
                            converted.insert(name.clone(), json!(false));
                        } else {
                            converted.insert(name.clone(), converted_schema);
                        }
                    }
                    out.insert(k.clone(), Value::Object(converted));
                }
                "items"
                | "contains"
                | "not"
                | "if"
                | "then"
                | "else"
                | "unevaluatedProperties"
                | "unevaluatedItems" => {
                    out.insert(k.clone(), self.schema(v, depth + 1, active, context)?);
                }
                "additionalProperties" => {
                    out.insert(
                        k.clone(),
                        if v.is_boolean() {
                            v.clone()
                        } else {
                            self.schema(v, depth + 1, active, context)?
                        },
                    );
                }
                "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                    let list = v.as_array().ok_or(fail("OPENAPI_SCHEMA_COMPOSITION"))?;
                    if list.len() > 64 {
                        return Err(fail("OPENAPI_SCHEMA_COMPOSITION_LIMIT"));
                    }
                    let values = list
                        .iter()
                        .map(|s| self.schema(s, depth + 1, active, context))
                        .collect::<Result<Vec<_>, _>>()?;
                    out.insert(k.clone(), json!(values));
                }
                "readOnly" | "writeOnly" => {
                    if !v.is_boolean() {
                        return Err(fail("OPENAPI_READ_WRITE_ANNOTATION_TYPE"));
                    }
                }
                "nullable" if self.legacy => {
                    if !v.is_boolean() {
                        return Err(fail("OPENAPI_NULLABLE_TYPE"));
                    }
                }
                "exclusiveMinimum" | "exclusiveMaximum" if self.legacy => {
                    if !v.is_boolean() {
                        return Err(fail("OPENAPI_EXCLUSIVE_BOUND_TYPE"));
                    }
                }
                "type" | "enum" | "const" | "required" | "minimum" | "maximum"
                | "exclusiveMinimum" | "exclusiveMaximum" | "multipleOf" | "minLength"
                | "maxLength" | "pattern" | "minItems" | "maxItems" | "uniqueItems"
                | "minContains" | "maxContains" | "minProperties" | "maxProperties"
                | "dependentRequired" | "format" => {
                    out.insert(k.clone(), v.clone());
                }
                _ => return Err(fail("OPENAPI_SCHEMA_KEYWORD_UNSUPPORTED")),
            }
        }
        if let Some(required) = out.get_mut("required").and_then(Value::as_array_mut) {
            required.retain(|n| !n.as_str().is_some_and(|n| forbidden.contains(n)));
        }
        if self.legacy {
            if m.get("nullable") == Some(&json!(true)) {
                if let Some(t) = m.get("type").and_then(Value::as_str) {
                    out.insert("type".into(), json!([t, "null"]));
                }
            }
            for (exclusive, inclusive) in [
                ("exclusiveMinimum", "minimum"),
                ("exclusiveMaximum", "maximum"),
            ] {
                if m.get(exclusive) == Some(&json!(true)) {
                    let n = m
                        .get(inclusive)
                        .filter(|v| v.is_number())
                        .ok_or(fail("OPENAPI_EXCLUSIVE_BOUND_REQUIRED"))?;
                    out.remove(inclusive);
                    out.insert(exclusive.into(), n.clone());
                }
            }
        }
        if let Some(format) = out
            .get("format")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            let range = match format.as_str() {
                "int32" => Some(json!({"type":"integer","minimum":i32::MIN,"maximum":i32::MAX})),
                "int64" => Some(json!({"type":"integer","minimum":i64::MIN,"maximum":i64::MAX})),
                _ => None,
            };
            if let Some(range) = range {
                if m["type"] != "integer" {
                    return Err(fail("OPENAPI_NUMERIC_FORMAT_TYPE"));
                }
                out.remove("format");
                out.entry("allOf".to_string())
                    .or_insert(json!([]))
                    .as_array_mut()
                    .ok_or(fail("OPENAPI_SCHEMA_COMPOSITION"))?
                    .push(range);
            } else if matches!(format.as_str(), "float" | "double") {
                if m["type"] != "number" {
                    return Err(fail("OPENAPI_NUMERIC_FORMAT_TYPE"));
                }
                out.remove("format");
                if format == "float" {
                    out.entry("allOf".to_string())
                        .or_insert(json!([]))
                        .as_array_mut()
                        .ok_or(fail("OPENAPI_SCHEMA_COMPOSITION"))?
                        .push(json!({"minimum":-(f32::MAX as f64),"maximum":f32::MAX as f64}));
                }
            } else if format == "password" {
                out.remove("format");
            } else if format == "byte" {
                if m["type"] != "string" {
                    return Err(fail("OPENAPI_BYTE_FORMAT_TYPE"));
                }
                out.remove("format");
                out.entry("allOf".to_string()).or_insert(json!([])).as_array_mut().ok_or(fail("OPENAPI_SCHEMA_COMPOSITION"))?.push(json!({"pattern":"^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$"}));
            }
        }
        Ok(Value::Object(out))
    }
    fn compiled_schema(&mut self, v: &Value, context: Context) -> Result<Value, String> {
        let s = self.schema(v, 0, &mut BTreeSet::new(), context)?;
        rest_profile::schema(&s)?;
        Ok(s)
    }
    fn security(&mut self, c: &Config, op: &Value) -> Result<(), String> {
        let Some(v) = op.get("security").or_else(|| self.doc.get("security")) else {
            return Ok(());
        };
        let choices = v.as_array().ok_or(fail("OPENAPI_SECURITY_ARRAY"))?;
        if choices.len() > 32 {
            return Err(fail("OPENAPI_SECURITY_LIMIT"));
        }
        if choices.is_empty() {
            return Ok(());
        }
        for choice in choices {
            let group = choice
                .as_object()
                .ok_or(fail("OPENAPI_SECURITY_REQUIREMENT"))?;
            if group.is_empty() {
                return Ok(());
            }
            let mut matched = true;
            for (name, scopes) in group {
                if !scopes.as_array().is_some_and(Vec::is_empty) {
                    matched = false;
                    break;
                }
                let scheme = self.doc["components"]["securitySchemes"]
                    .get(name)
                    .ok_or(fail("OPENAPI_SECURITY_SCHEME_MISSING"))?
                    .clone();
                let s = self.object(&scheme)?;
                let yes = match (s["type"].as_str(), s["scheme"].as_str(), &c.auth) {
                    (Some("http"), Some("bearer"), Auth::Bearer { .. })
                    | (Some("http"), Some("basic"), Auth::Basic { .. }) => true,
                    (Some("apiKey"), _, _) if s["in"] == "header" => {
                        let h = s["name"].as_str().unwrap_or("").to_ascii_lowercase();
                        matches!(&c.auth,Auth::Header{header,..} if header==&h)
                            || c.headers.get(&h).is_some_and(|h| h.secret_ref.is_some())
                    }
                    _ => false,
                };
                matched &= yes;
            }
            if matched {
                return Ok(());
            }
        }
        Err(fail("OPENAPI_SECURITY_NOT_CONFIGURED_OR_UNSUPPORTED"))
    }
    fn parameters(&mut self, path: &Value, op: &Value) -> Result<(Value, Value), String> {
        let mut params = BTreeMap::new();
        for layer in [path, op] {
            let Some(items) = layer.get("parameters") else {
                continue;
            };
            let items = items.as_array().ok_or(fail("OPENAPI_PARAMETERS_ARRAY"))?;
            if items.len() > 128 {
                return Err(fail("OPENAPI_PARAMETER_LIMIT"));
            }
            let mut own = BTreeSet::new();
            for item in items {
                let p = self.object(item)?;
                let key = (
                    p["in"]
                        .as_str()
                        .ok_or(fail("OPENAPI_PARAMETER_LOCATION"))?
                        .to_string(),
                    p["name"]
                        .as_str()
                        .ok_or(fail("OPENAPI_PARAMETER_NAME"))?
                        .to_string(),
                );
                if !own.insert(key.clone()) {
                    return Err(fail("OPENAPI_PARAMETER_DUPLICATE"));
                }
                params.insert(key, p);
            }
        }
        let mut path_props = Map::new();
        let mut query_props = Map::new();
        let mut path_req = Vec::new();
        let mut query_req = Vec::new();
        for ((location, name), p) in params {
            let required = p
                .get("required")
                .map(|v| v.as_bool().ok_or(fail("OPENAPI_PARAMETER_REQUIRED_TYPE")))
                .transpose()?
                .unwrap_or(false);
            let style = p
                .get("style")
                .and_then(Value::as_str)
                .unwrap_or(if location == "path" { "simple" } else { "form" });
            if p.get("content").is_some()
                || p["allowReserved"] == true
                || !matches!(
                    (location.as_str(), style),
                    ("path", "simple") | ("query", "form")
                )
            {
                return Err(fail("OPENAPI_PARAMETER_SERIALIZATION_UNSUPPORTED"));
            }
            let s = self.compiled_schema(
                p.get("schema").ok_or(fail("OPENAPI_PARAMETER_SCHEMA"))?,
                Context::Parameter,
            )?;
            match location.as_str() {
                "path" if required => {
                    path_req.push(name.clone());
                    path_props.insert(name, s);
                }
                "query" => {
                    if required {
                        query_req.push(name.clone());
                    }
                    query_props.insert(name, s);
                }
                _ => return Err(fail("OPENAPI_PARAMETER_LOCATION_OR_REQUIRED")),
            }
        }
        Ok((
            json!({"type":"object","properties":path_props,"required":path_req,"additionalProperties":false}),
            json!({"type":"object","properties":query_props,"required":query_req,"additionalProperties":false}),
        ))
    }
    fn media(content: &Value) -> Result<(&str, &Value), String> {
        let m = content.as_object().ok_or(fail("OPENAPI_MEDIA_OBJECT"))?;
        if let Some(v) = m.get("application/json") {
            return Ok(("application/json", v));
        }
        if let Some((k, v)) = m.iter().find(|(k, _)| rest_profile::json_media(k)) {
            return Ok((k, v));
        }
        if let Some(v) = m.get("text/plain") {
            return Ok(("text/plain", v));
        }
        Err(fail("OPENAPI_MEDIA_UNSUPPORTED"))
    }
    fn response(&mut self, v: &Value, head: bool) -> Result<ResponseSpec, String> {
        let r = self.object(v)?;
        if head || r.get("content").is_none() {
            return Ok(ResponseSpec {
                response: ResponseKind::Empty,
                schema: None,
                content_type: None,
            });
        }
        let (media, m) = Self::media(&r["content"])?;
        if media == "text/plain" {
            if m.get("schema").is_some_and(|s| {
                s["type"] != "string"
                    || s.as_object().is_none_or(|m| {
                        m.keys()
                            .any(|k| !matches!(k.as_str(), "type" | "description" | "title"))
                    })
            }) {
                return Err(fail("OPENAPI_TEXT_SCHEMA_UNSUPPORTED"));
            }
            return Ok(ResponseSpec {
                response: ResponseKind::Text,
                schema: None,
                content_type: Some(media.into()),
            });
        }
        let schema = m
            .get("schema")
            .map(|s| self.compiled_schema(s, Context::Response))
            .transpose()?;
        Ok(ResponseSpec {
            response: ResponseKind::Json,
            schema,
            content_type: Some(media.into()),
        })
    }
    fn operation(
        &mut self,
        c: &Config,
        path: &Value,
        op: &Value,
        method: &str,
        route: &str,
        source: Option<&reqwest::Url>,
    ) -> Result<Operation, String> {
        self.security(c, op)?;
        server(
            c,
            op.get("servers")
                .or_else(|| path.get("servers"))
                .or_else(|| self.doc.get("servers")),
            source,
        )?;
        let (path_schema, query_schema) = self.parameters(path, op)?;
        let (body_schema, body_required, request_content_type) =
            if let Some(v) = op.get("requestBody") {
                let b = self.object(v)?;
                let (media, m) = Self::media(&b["content"])?;
                if !rest_profile::json_media(media) {
                    return Err(fail("OPENAPI_REQUEST_MEDIA_UNSUPPORTED"));
                }
                let required = b
                    .get("required")
                    .map(|v| v.as_bool().ok_or(fail("OPENAPI_BODY_REQUIRED_TYPE")))
                    .transpose()?
                    .unwrap_or(false);
                (
                    Some(self.compiled_schema(
                        m.get("schema").ok_or(fail("OPENAPI_BODY_SCHEMA"))?,
                        Context::Request,
                    )?),
                    required,
                    Some(media.to_string()),
                )
            } else {
                (None, false, None)
            };
        let raw = op["responses"]
            .as_object()
            .ok_or(fail("OPENAPI_RESPONSES_REQUIRED"))?;
        let mut responses = BTreeMap::new();
        if let Some(v) = raw.get("2XX") {
            let r = self.response(v, method == "HEAD")?;
            for status in 200..=299 {
                responses.insert(status, r.clone());
            }
        }
        for (status, v) in raw {
            if let Ok(status) = status.parse::<u16>() {
                if (200..=299).contains(&status) {
                    responses.insert(status, self.response(v, method == "HEAD")?);
                }
            }
        }
        let first = responses
            .values()
            .next()
            .ok_or(fail("OPENAPI_EXPLICIT_SUCCESS_RESPONSE_REQUIRED"))?
            .clone();
        Ok(Operation {
            method: method.into(),
            path: route.into(),
            path_schema,
            query_schema,
            body_schema,
            body_required,
            request_content_type,
            response_schema: None,
            response: first.response,
            success_statuses: responses.keys().copied().collect(),
            responses,
            risk: Risk::Mandatory,
        })
    }
}
fn server(c: &Config, v: Option<&Value>, source: Option<&reqwest::Url>) -> Result<(), String> {
    let fallback = json!([{"url":"/"}]);
    let servers = v
        .unwrap_or(&fallback)
        .as_array()
        .ok_or(fail("OPENAPI_SERVERS_ARRAY"))?;
    if servers.len() > 16 {
        return Err(fail("OPENAPI_SERVERS_LIMIT"));
    }
    let base = rest_profile::base(c)?;
    for s in if servers.is_empty() {
        fallback.as_array().unwrap()
    } else {
        servers
    } {
        if s.get("variables").is_some() {
            continue;
        }
        let Some(text) = s["url"].as_str() else {
            continue;
        };
        let url = reqwest::Url::parse(text).ok().or_else(|| {
            if text.starts_with('/') {
                base.join(text).ok()
            } else {
                source.and_then(|s| s.join(text).ok())
            }
        });
        let Some(mut url) = url else { continue };
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            continue;
        }
        if !url.path().ends_with('/') {
            let p = format!("{}/", url.path());
            url.set_path(&p);
        }
        if url == base {
            return Ok(());
        }
    }
    Err(fail("OPENAPI_SERVER_OUT_OF_PROFILE_OR_UNRESOLVED"))
}
pub fn preview(
    doc: &Value,
    current: &Config,
    source: Option<&reqwest::Url>,
) -> Result<Preview, String> {
    if doc.to_string().len() > 2 * 1024 * 1024 {
        return Err(fail("OPENAPI_DOCUMENT_LIMIT"));
    }
    let version = doc["openapi"].as_str().ok_or(fail("OPENAPI_VERSION"))?;
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts[0] != "3"
        || !matches!(parts[1], "0" | "1" | "2")
        || parts[2].parse::<u16>().is_err()
    {
        return Err(fail("OPENAPI_VERSION_UNSUPPORTED"));
    }
    if !doc["info"]["title"].is_string() || !doc["info"]["version"].is_string() {
        return Err(fail("OPENAPI_INFO_REQUIRED"));
    }
    let base = rest_profile::base(current)?;
    if source.is_some_and(|s| {
        s.origin() != base.origin()
            || !s.path().starts_with(base.path())
            || !s.username().is_empty()
            || s.password().is_some()
            || s.query().is_some()
            || s.fragment().is_some()
    }) {
        return Err(fail("OPENAPI_SOURCE_OUT_OF_PROFILE"));
    }
    let paths = doc["paths"]
        .as_object()
        .ok_or(fail("OPENAPI_PATHS_REQUIRED"))?;
    if paths.len() > 256 {
        return Err(fail("OPENAPI_PATH_LIMIT"));
    }
    let mut converter = Convert {
        doc,
        legacy: parts[1] == "0",
        nodes: 0,
    };
    let mut profile = current.clone();
    profile.read_only = true;
    profile.operations.clear();
    let mut diagnostics = Vec::new();
    let mut seen = 0;
    let mut ids = BTreeSet::new();
    for (route, path) in paths {
        if route.starts_with("x-") {
            continue;
        }
        let path = match converter.object(path) {
            Ok(p) => p,
            Err(code) => {
                diagnostics.push(Diagnostic {
                    path: route.chars().take(2048).collect(),
                    method: "*".into(),
                    code,
                });
                continue;
            }
        };
        let fixed = [
            "get", "head", "post", "put", "patch", "delete", "options", "trace", "query",
        ];
        if parts[1] != "2"
            && (path.get("query").is_some() || path.get("additionalOperations").is_some())
        {
            return Err(fail("OPENAPI_METHOD_FIELD_VERSION_UNSUPPORTED"));
        }
        let mut entries: Vec<_> = fixed
            .iter()
            .filter_map(|method| {
                path.get(*method)
                    .map(|op| (method.to_ascii_uppercase(), op))
            })
            .collect();
        if let Some(extra) = path.get("additionalOperations") {
            let extra = extra
                .as_object()
                .ok_or(fail("OPENAPI_ADDITIONAL_OPERATIONS_OBJECT"))?;
            for (method, op) in extra {
                if fixed.iter().any(|m| m.eq_ignore_ascii_case(method)) {
                    return Err(fail("OPENAPI_ADDITIONAL_METHOD_CONFLICT"));
                }
                entries.push((method.clone(), op));
            }
        }
        for (method, op) in entries {
            seen += 1;
            if seen > 256 {
                return Err(fail("OPENAPI_OPERATION_LIMIT"));
            }
            let id = op["operationId"]
                .as_str()
                .filter(|s| atom(s))
                .map(str::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "op_{:x}",
                        Sha256::digest(format!("{method}:{route}").as_bytes())
                    )[..35]
                        .to_string()
                });
            if !ids.insert(id.clone()) {
                return Err(fail("OPENAPI_OPERATION_ID_DUPLICATE"));
            }
            let result = converter
                .operation(current, &path, op, &method, route, source)
                .and_then(|operation| {
                    let mut check = current.clone();
                    check.operations = BTreeMap::from([(id.clone(), operation.clone())]);
                    rest_profile::config(&json!(check))?;
                    Ok(operation)
                });
            match result {
                Ok(operation) => {
                    profile.operations.insert(id, operation);
                }
                Err(code) => diagnostics.push(Diagnostic {
                    path: route.chars().take(2048).collect(),
                    method,
                    code,
                }),
            }
        }
    }
    rest_profile::config(&json!(profile))?;
    Ok(Preview {
        version: version.into(),
        operations_proposed: profile.operations.len(),
        operations_seen: seen,
        profile,
        diagnostics,
        automatically_enabled: false,
    })
}
pub fn select(preview: &Preview, ids: &[String]) -> Result<Config, String> {
    if ids.is_empty() || ids.len() > 128 || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
        return Err(fail("OPENAPI_EXPLICIT_SELECTION_REQUIRED"));
    }
    let mut selected = preview.profile.clone();
    selected.operations.clear();
    for id in ids {
        selected.operations.insert(
            id.clone(),
            preview
                .profile
                .operations
                .get(id)
                .ok_or(fail("OPENAPI_OPERATION_NOT_PROPOSED"))?
                .clone(),
        );
    }
    rest_profile::config(&json!(selected))?;
    Ok(selected)
}
