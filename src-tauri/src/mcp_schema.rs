//! Bounded, offline validation of untrusted server schemas. Schemas describe
//! messages, never grant authority or trigger HTTP/file retrieval.
use super::protocol;
use serde_json::Value;

struct Offline;
impl jsonschema::Retrieve for Offline {
    fn retrieve(
        &self,
        _: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("MCP_SCHEMA_EXTERNAL_REFERENCE".into())
    }
}
pub(crate) fn bounded(value: &Value, bytes: usize) -> Result<(), String> {
    if value.to_string().len() > bytes {
        return Err("MCP_VALUE_LIMIT".into());
    }
    fn walk(v: &Value, depth: usize, nodes: &mut usize) -> Result<(), String> {
        *nodes += 1;
        if depth > 32 || *nodes > 8192 {
            return Err("MCP_VALUE_LIMIT".into());
        }
        match v {
            Value::Object(m) => {
                for child in m.values() {
                    walk(child, depth + 1, nodes)?;
                }
            }
            Value::Array(a) => {
                for child in a {
                    walk(child, depth + 1, nodes)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    walk(value, 0, &mut 0)
}
pub(super) fn validator(schema: &Value) -> Result<jsonschema::Validator, String> {
    bounded(schema, 65536)?;
    if !schema.is_object() || schema["type"] != "object" {
        return Err("MCP_SCHEMA_OBJECT".into());
    }
    // Draft detection is local. Unknown meta-schemas fail without retrieval.
    if let Some(uri) = schema.get("$schema") {
        if !uri.as_str().is_some_and(|s| {
            matches!(
                s,
                "https://json-schema.org/draft/2020-12/schema"
                    | "https://json-schema.org/draft/2019-09/schema"
                    | "http://json-schema.org/draft-07/schema#"
                    | "http://json-schema.org/draft-06/schema#"
                    | "http://json-schema.org/draft-04/schema#"
            )
        }) {
            return Err("MCP_SCHEMA_DIALECT_UNSUPPORTED".into());
        }
    }
    jsonschema::options()
        .with_retriever(Offline)
        .with_pattern_options(
            jsonschema::PatternOptions::fancy_regex()
                .backtrack_limit(100_000)
                .size_limit(65536),
        )
        .build(schema)
        .map_err(|_| "MCP_SCHEMA_INVALID_OR_EXTERNAL".into())
}
pub(crate) struct Tool {
    pub definition: Value,
    input: jsonschema::Validator,
    output: Option<jsonschema::Validator>,
    pub fields: Vec<protocol::HeaderField>,
}
impl Tool {
    pub fn new(v: &Value, mirrored: bool) -> Result<Self, String> {
        bounded(v, 128 * 1024)?;
        if !v.is_object()
            || v["name"]
                .as_str()
                .is_none_or(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err("MCP_TOOL_DEFINITION".into());
        }
        Ok(Self {
            definition: v.clone(),
            input: validator(&v["inputSchema"])?,
            output: v.get("outputSchema").map(validator).transpose()?,
            fields: if mirrored {
                protocol::fields(&v["inputSchema"])?
            } else {
                vec![]
            },
        })
    }
    pub fn arguments(&self, args: &Value) -> Result<(), String> {
        bounded(args, 65536)?;
        if !args.is_object() || !self.input.is_valid(args) {
            return Err("MCP_TOOL_INPUT_SCHEMA".into());
        }
        Ok(())
    }
    pub fn output(&self, result: &Value) -> Result<(), String> {
        if !result["content"].is_array() {
            return Err("MCP_TOOL_CONTENT".into());
        }
        if let Some(schema) = &self.output {
            let value = result
                .get("structuredContent")
                .ok_or("MCP_TOOL_OUTPUT_SCHEMA")?;
            bounded(value, 1024 * 1024)?;
            if !schema.is_valid(value) {
                return Err("MCP_TOOL_OUTPUT_SCHEMA".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "mcp_schema_tests.rs"]
mod tests;
