//! Modern subscriptions/listen wire contract. Transport and Core remain its callers.
//! Only opt-in correlated events survive; caller must close on every error.
use crate::mcp_client::{protocol, schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub(crate) const FRAME_LIMIT: usize = 64 * 1024;
pub(crate) const SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";
fn is_false(v: &bool) -> bool {
    !v
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct Filter {
    #[serde(default, skip_serializing_if = "is_false")]
    pub tools_list_changed: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub prompts_list_changed: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub resources_list_changed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resource_subscriptions: Vec<String>,
}
impl Filter {
    pub(crate) fn parse(v: &Value, require_nonempty: bool) -> Result<Self, String> {
        schema::bounded(v, FRAME_LIMIT)?;
        let f: Self = serde_json::from_value(v.clone()).map_err(|_| "MCP_LISTEN_FILTER")?;
        if f.resource_subscriptions.len() > 32 {
            return Err("MCP_LISTEN_URI_LIMIT".into());
        }
        let mut seen = BTreeSet::new();
        for uri in &f.resource_subscriptions {
            if uri.is_empty()
                || uri.len() > 8192
                || uri.chars().any(char::is_control)
                || reqwest::Url::parse(uri).is_err()
                || !seen.insert(uri)
            {
                return Err("MCP_LISTEN_URI".into());
            }
        }
        if require_nonempty && f.is_empty() {
            return Err("MCP_LISTEN_FILTER_EMPTY".into());
        }
        Ok(f)
    }
    pub(crate) fn is_empty(&self) -> bool {
        !self.tools_list_changed
            && !self.prompts_list_changed
            && !self.resources_list_changed
            && self.resource_subscriptions.is_empty()
    }
    fn subset_of(&self, requested: &Self) -> bool {
        (!self.tools_list_changed || requested.tools_list_changed)
            && (!self.prompts_list_changed || requested.prompts_list_changed)
            && (!self.resources_list_changed || requested.resources_list_changed)
            && self
                .resource_subscriptions
                .iter()
                .all(|u| requested.resource_subscriptions.contains(u))
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Acknowledged(Filter),
    ToolsChanged,
    PromptsChanged,
    ResourcesChanged,
    ResourceUpdated(String),
    Complete,
}
pub(crate) struct Protocol {
    id: Value,
    requested: Filter,
    honored: Option<Filter>,
    completed: bool,
}
impl Protocol {
    pub(crate) fn new(id: Value, requested: Filter) -> Result<Self, String> {
        // Validate even a caller-constructed Filter before it becomes authority.
        let requested = Filter::parse(
            &serde_json::to_value(requested).map_err(|_| "MCP_LISTEN_FILTER")?,
            true,
        )?;
        if !(id.as_i64().is_some()
            || id.as_str().is_some_and(|s| {
                !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
            }))
        {
            return Err("MCP_LISTEN_REQUEST_ID".into());
        }
        if requested.is_empty() {
            return Err("MCP_LISTEN_FILTER_EMPTY".into());
        }
        Ok(Self {
            id,
            requested,
            honored: None,
            completed: false,
        })
    }
    pub(crate) fn accept(&mut self, v: &Value) -> Result<Event, String> {
        if self.completed {
            return Err("MCP_LISTEN_AFTER_COMPLETE".into());
        }
        schema::bounded(v, FRAME_LIMIT)?;
        if !v.is_object() || v["jsonrpc"] != "2.0" {
            return Err("MCP_LISTEN_ENVELOPE".into());
        }
        if v.get("id").is_some() {
            let result = match protocol::reply(v, &self.id)? {
                protocol::Reply::Error(_) => return Err("MCP_LISTEN_REMOTE_ERROR".into()),
                protocol::Reply::Result(result) => result,
            };
            if self.honored.is_none() {
                return Err("MCP_LISTEN_ACK_REQUIRED".into());
            }
            protocol::complete(v, &self.id, true)?;
            if result["_meta"][SUBSCRIPTION_ID] != self.id {
                return Err("MCP_LISTEN_CORRELATION".into());
            }
            // Completion has no subscription-specific payload. Standard metadata
            // extensions may be ignored but cannot become an event or authority.
            if result
                .as_object()
                .unwrap()
                .keys()
                .any(|k| k != "resultType" && k != "_meta")
            {
                return Err("MCP_LISTEN_FINAL_PAYLOAD".into());
            }
            self.completed = true;
            return Ok(Event::Complete);
        }
        if v.get("result").is_some() || v.get("error").is_some() || !v["params"].is_object() {
            return Err("MCP_LISTEN_ENVELOPE".into());
        }
        let method = v["method"].as_str().ok_or("MCP_LISTEN_ENVELOPE")?;
        let params = &v["params"];
        if params["_meta"][SUBSCRIPTION_ID] != self.id {
            return Err("MCP_LISTEN_CORRELATION".into());
        }
        if method == "notifications/subscriptions/acknowledged" {
            if self.honored.is_some() {
                return Err("MCP_LISTEN_DUPLICATE_ACK".into());
            }
            let honored = Filter::parse(&params["notifications"], false)?;
            if !honored.subset_of(&self.requested) {
                return Err("MCP_LISTEN_ACK_SCOPE".into());
            }
            self.honored = Some(honored.clone());
            return Ok(Event::Acknowledged(honored));
        }
        let honored = self.honored.as_ref().ok_or("MCP_LISTEN_ACK_REQUIRED")?;
        match method {
            "notifications/tools/list_changed" if honored.tools_list_changed => {
                Ok(Event::ToolsChanged)
            }
            "notifications/prompts/list_changed" if honored.prompts_list_changed => {
                Ok(Event::PromptsChanged)
            }
            "notifications/resources/list_changed" if honored.resources_list_changed => {
                Ok(Event::ResourcesChanged)
            }
            "notifications/resources/updated" => {
                let uri = params["uri"].as_str().ok_or("MCP_LISTEN_URI")?;
                if !honored.resource_subscriptions.iter().any(|u| u == uri) {
                    return Err("MCP_LISTEN_EVENT_SCOPE".into());
                }
                Ok(Event::ResourceUpdated(uri.into()))
            }
            _ => Err("MCP_LISTEN_EVENT_SCOPE".into()),
        }
    }
    pub(crate) fn eof(&self) -> Result<(), String> {
        if self.completed {
            Ok(())
        } else {
            Err("MCP_LISTEN_ABRUPT_EOF".into())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn requested() -> Filter {
        Filter::parse(
            &json!({"toolsListChanged":true,"resourceSubscriptions":["owned:///one"]}),
            true,
        )
        .unwrap()
    }
    fn notice(method: &str, params: Value) -> Value {
        let mut params = params;
        params["_meta"] = json!({SUBSCRIPTION_ID:2});
        json!({"jsonrpc":"2.0","method":method,"params":params})
    }
    fn ack() -> Value {
        notice(
            "notifications/subscriptions/acknowledged",
            json!({"notifications":{"toolsListChanged":true,"resourceSubscriptions":["owned:///one"]}}),
        )
    }
    #[test]
    fn closed_nonempty_filter_requires_unique_bounded_uri_and_correct_types() {
        for value in [
            json!({}),
            json!({"toolsListChanged":false}),
            json!({"sampling":true}),
            json!({"toolsListChanged":"true"}),
            json!({"resourceSubscriptions":["owned:///one","owned:///one"]}),
            json!({"resourceSubscriptions":["broken"]}),
        ] {
            assert!(Filter::parse(&value, true).is_err());
        }
        assert!(Filter::parse(&json!({}), false).unwrap().is_empty());
        assert!(Filter::parse(
            &json!({"resourceSubscriptions":vec!["owned:///one";33]}),
            false
        )
        .is_err());
        for uris in [
            vec!["owned:///one".to_string(); 33],
            vec!["owned:///one".into(), "owned:///one".into()],
            vec!["broken".into()],
        ] {
            assert!(Protocol::new(
                json!(2),
                Filter {
                    resource_subscriptions: uris,
                    ..Filter::default()
                },
            )
            .is_err());
        }
    }
    #[test]
    fn acknowledgment_order_subset_and_exact_typed_id_are_enforced() {
        let mut p = Protocol::new(json!(2), requested()).unwrap();
        assert_eq!(
            p.accept(&notice("notifications/tools/list_changed", json!({})))
                .unwrap_err(),
            "MCP_LISTEN_ACK_REQUIRED"
        );
        let mut wrong = ack();
        wrong["params"]["_meta"][SUBSCRIPTION_ID] = json!("2");
        assert_eq!(p.accept(&wrong).unwrap_err(), "MCP_LISTEN_CORRELATION");
        let extra = notice(
            "notifications/subscriptions/acknowledged",
            json!({"notifications":{"promptsListChanged":true}}),
        );
        assert_eq!(p.accept(&extra).unwrap_err(), "MCP_LISTEN_ACK_SCOPE");
        assert_eq!(p.accept(&ack()).unwrap(), Event::Acknowledged(requested()));
        assert_eq!(p.accept(&ack()).unwrap_err(), "MCP_LISTEN_DUPLICATE_ACK");
    }
    #[test]
    fn honored_filter_does_not_authorize_omitted_types_uris_or_server_requests() {
        let mut p = Protocol::new(json!(2), requested()).unwrap();
        p.accept(&notice(
            "notifications/subscriptions/acknowledged",
            json!({"notifications":{"toolsListChanged":true}}),
        ))
        .unwrap();
        assert_eq!(
            p.accept(&notice("notifications/tools/list_changed", json!({})))
                .unwrap(),
            Event::ToolsChanged
        );
        for event in [
            notice(
                "notifications/resources/updated",
                json!({"uri":"owned:///one"}),
            ),
            notice("notifications/progress", json!({})),
            notice("notifications/prompts/list_changed", json!({})),
            notice("sampling/createMessage", json!({})),
        ] {
            assert_eq!(p.accept(&event).unwrap_err(), "MCP_LISTEN_EVENT_SCOPE");
        }
        let request = json!({"jsonrpc":"2.0","id":2,"method":"sampling/createMessage","params":{}});
        assert_eq!(p.accept(&request).unwrap_err(), "MCP_RESPONSE_SCOPE");
    }
    #[test]
    fn exact_resource_notifications_and_graceful_complete_are_distinct_from_eof() {
        let mut p = Protocol::new(json!(2), requested()).unwrap();
        p.accept(&ack()).unwrap();
        assert_eq!(
            p.accept(&notice(
                "notifications/resources/updated",
                json!({"uri":"owned:///one"})
            ))
            .unwrap(),
            Event::ResourceUpdated("owned:///one".into())
        );
        assert_eq!(
            p.accept(&notice(
                "notifications/resources/updated",
                json!({"uri":"owned:///two"})
            ))
            .unwrap_err(),
            "MCP_LISTEN_EVENT_SCOPE"
        );
        assert_eq!(p.eof().unwrap_err(), "MCP_LISTEN_ABRUPT_EOF");
        let mut final_value = json!({"jsonrpc":"2.0","id":2,"result":{"resultType":"complete","_meta":{SUBSCRIPTION_ID:3}}});
        assert_eq!(
            p.accept(&final_value).unwrap_err(),
            "MCP_LISTEN_CORRELATION"
        );
        final_value["result"]["_meta"][SUBSCRIPTION_ID] = json!(2);
        assert_eq!(p.accept(&final_value).unwrap(), Event::Complete);
        p.eof().unwrap();
        assert_eq!(p.accept(&ack()).unwrap_err(), "MCP_LISTEN_AFTER_COMPLETE");
    }
    #[test]
    fn empty_ack_is_visible_and_bounded_or_ambiguous_frames_never_become_events() {
        let mut p = Protocol::new(json!(2), requested()).unwrap();
        assert_eq!(
            p.accept(&notice(
                "notifications/subscriptions/acknowledged",
                json!({"notifications":{}})
            ))
            .unwrap(),
            Event::Acknowledged(Filter::default())
        );
        assert_eq!(
            p.accept(&notice("notifications/tools/list_changed", json!({})))
                .unwrap_err(),
            "MCP_LISTEN_EVENT_SCOPE"
        );
        let mut oversized = notice(
            "notifications/tools/list_changed",
            json!({"extra":"x".repeat(FRAME_LIMIT)}),
        );
        assert!(p.accept(&oversized).is_err());
        oversized["params"] = json!({});
        oversized["result"] = json!({});
        assert_eq!(p.accept(&oversized).unwrap_err(), "MCP_LISTEN_ENVELOPE");
    }
}
