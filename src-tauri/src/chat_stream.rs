//! Keep provider messages separate; token deltas only extend their own message.
use crate::{runtime::Core, storage};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct Replies {
    pub rows: Vec<Value>,
    current: Option<usize>,
    blocks: BTreeMap<u64, String>,
}
impl Replies {
    fn begin(&mut self) {
        self.rows.push(json!({"id":storage::id(),"role":"assistant","content_text":"","status":"streaming","created_at":storage::now()}));
        self.current = Some(self.rows.len() - 1);
        self.blocks.clear();
    }
    pub fn accept(&mut self, value: &Value) -> Option<Value> {
        let event = &value["event"];
        if value["type"] == "stream_event" {
            match event["type"].as_str()? {
                "message_start" => self.begin(),
                "content_block_start" if event["content_block"]["type"] == "text" => {
                    if self.current.is_none() {
                        self.begin();
                    }
                    self.blocks.insert(
                        event["index"].as_u64().unwrap_or(0),
                        event["content_block"]["text"].as_str().unwrap_or("").into(),
                    );
                }
                "content_block_delta" if event["delta"]["type"] == "text_delta" => {
                    if self.current.is_none() {
                        self.begin();
                    }
                    self.blocks
                        .entry(event["index"].as_u64().unwrap_or(0))
                        .or_default()
                        .push_str(event["delta"]["text"].as_str().unwrap_or(""));
                }
                _ => return None,
            }
            let index = self.current?;
            self.rows[index]["content_text"] = json!(self
                .blocks
                .values()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n\n"));
        } else if value["type"] == "assistant" {
            let text = value["message"]["content"]
                .as_array()?
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            if self.current.is_none() {
                self.begin();
            }
            let index = self.current?;
            self.rows[index]["content_text"] = json!(text);
            self.rows[index]["status"] = json!("complete");
            self.current = None;
            return Some(self.rows[index].clone());
        } else if value["type"] == "result" && self.text().is_empty() {
            let text = value["result"].as_str().unwrap_or("");
            if text.is_empty() {
                return None;
            }
            self.begin();
            let index = self.current?;
            self.rows[index]["content_text"] = json!(text);
            self.rows[index]["status"] = json!("complete");
        } else {
            return None;
        }
        self.current.map(|i| self.rows[i].clone())
    }
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .filter_map(|r| r["content_text"].as_str())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
    pub fn persist(&self, core: &Core, conversation: &str, status: &str) -> Result<(), String> {
        for row in &self.rows {
            if row["content_text"].as_str().unwrap_or("").is_empty() {
                continue;
            }
            core.store.execute("INSERT INTO messages(id,conversation_id,role,content_text,status,created_at) VALUES(?,?,'assistant',?,?,?) ON CONFLICT(id) DO UPDATE SET content_text=excluded.content_text,status=excluded.status", &[row["id"].clone(),json!(conversation),row["content_text"].clone(),json!(if row["status"]=="complete" {"complete"} else {status}),row["created_at"].clone()])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn responses_do_not_merge_across_tool_rounds_or_duplicate_final_text() {
        let mut replies = Replies::default();
        let start = json!({"type":"stream_event","event":{"type":"message_start"}});
        replies.accept(&start);
        let delta = |i, text| json!({"type":"stream_event","event":{"type":"content_block_delta","index":i,"delta":{"type":"text_delta","text":text}}});
        let first = replies.accept(&delta(0, "Voy a comprobarlo.")).unwrap();
        replies.accept(&json!({"type":"assistant","message":{"content":[{"type":"text","text":"Voy a comprobarlo."},{"type":"tool_use","name":"test"}]}}));
        replies.accept(&start);
        replies.accept(&delta(0, "Hecho, "));
        replies.accept(&delta(0, "señor."));
        let second = replies.accept(&delta(1, "Detalles.")).unwrap();
        assert_ne!(first["id"], second["id"]);
        assert!(first["created_at"].is_string());
        assert_eq!(
            replies.text(),
            "Voy a comprobarlo.\n\nHecho, señor.\n\nDetalles."
        );
        replies.accept(&json!({"type":"result","result":"Hecho, señor."}));
        assert_eq!(replies.rows.len(), 2);
    }
}
