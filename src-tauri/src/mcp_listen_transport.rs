//! One modern request owns one bounded notification stream. No replay or resume.
use super::*;
use crate::{mcp_request_guard::Guard, mcp_subscription_wire::FRAME_LIMIT};
use std::{collections::VecDeque, time::Instant};

struct HttpReader {
    response: reqwest::Response,
    decoder: super::super::sse::Decoder,
    total: usize,
    chunk: Vec<u8>,
    position: usize,
    frames: VecDeque<String>,
}
pub(crate) struct Listening {
    session: Session,
    guard: Guard,
    http: Option<HttpReader>,
}
impl Session {
    pub(crate) async fn listen(
        mut self,
        id: Value,
        notifications: Value,
        guard: Guard,
    ) -> Result<Listening, String> {
        if !matches!(self.era, Era::Modern) {
            return Err("MCP_LISTEN_MODERN_REQUIRED".into());
        }
        let request = protocol::modern_request(
            &json!({"jsonrpc":"2.0","id":id,"method":"subscriptions/listen","params":{"notifications":notifications}}),
        )?;
        guard.check()?;
        let http = match &mut self.transport {
            Transport::Stdio(stdio) => {
                stdio.pending = Some((request["id"].clone(), "subscriptions/listen".into()));
                Guard::run(Some(&guard), stdio.write(&request)).await?;
                None
            }
            Transport::Http(http) => {
                let response = Guard::run(Some(&guard), http.post(&request, &[], true)).await?;
                if !response.status().is_success() {
                    return Err(format!("HTTP_STATUS_{}", response.status().as_u16()));
                }
                if !response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| {
                        v.split(';')
                            .next()
                            .unwrap_or("")
                            .trim()
                            .eq_ignore_ascii_case("text/event-stream")
                    })
                {
                    return Err("MCP_LISTEN_SSE_REQUIRED".into());
                }
                Some(HttpReader {
                    response,
                    decoder: super::super::sse::Decoder::bounded(FRAME_LIMIT)?,
                    total: 0,
                    chunk: vec![],
                    position: 0,
                    frames: VecDeque::new(),
                })
            }
        };
        guard.check()?;
        Ok(Listening {
            session: self,
            guard,
            http,
        })
    }
}
impl Listening {
    pub(crate) async fn next(&mut self) -> Result<Value, String> {
        self.guard.check()?;
        let Some(http) = &mut self.http else {
            return match &mut self.session.transport {
                Transport::Stdio(stdio) => stdio.frame(FRAME_LIMIT, Some(&self.guard)).await,
                _ => Err("MCP_LISTEN_TRANSPORT".into()),
            };
        };
        let mut checked = Instant::now();
        loop {
            if checked.elapsed() >= Duration::from_millis(50) {
                self.guard.check()?;
                checked = Instant::now();
            }
            if let Some(frame) = http.frames.pop_front() {
                self.guard.check()?;
                if frame.len() > FRAME_LIMIT {
                    return Err("MCP_LISTEN_FRAME_LIMIT".into());
                }
                return serde_json::from_str(&frame).map_err(|_| "MCP_JSON".into());
            }
            if http.position < http.chunk.len() {
                let end = (http.position + 4096).min(http.chunk.len());
                let frames =
                    http.decoder
                        .push(&http.chunk[http.position..end])
                        .map_err(|error| {
                            if error == "HTTP_RESPONSE_LIMIT" {
                                "MCP_LISTEN_FRAME_LIMIT".to_string()
                            } else {
                                error
                            }
                        })?;
                http.position = end;
                http.frames.extend(frames);
                // Parsing alone grants no authority. Yield between bounded
                // slices so outer revocation/cancel/lease checks can run even
                // when every network read is immediately ready.
                tokio::task::yield_now().await;
                continue;
            }
            let chunk = http
                .response
                .chunk()
                .await
                .map_err(|_| "MCP_NETWORK")?
                .ok_or("MCP_LISTEN_ABRUPT_EOF")?;
            http.total = http
                .total
                .checked_add(chunk.len())
                .ok_or("MCP_RESPONSE_LIMIT")?;
            if http.total > LIMIT {
                return Err("MCP_RESPONSE_LIMIT".into());
            }
            http.chunk = chunk.to_vec();
            http.position = 0;
        }
    }
    pub(crate) fn completed(&mut self) {
        if let Transport::Stdio(stdio) = &mut self.session.transport {
            stdio.pending = None;
        }
    }
    pub(crate) async fn close(mut self) {
        // Revocation never authorizes an additional cancellation notification.
        // EOF and exact owned process/job termination still happen locally.
        if self.guard.check().is_err() {
            if let Transport::Stdio(stdio) = &mut self.session.transport {
                stdio.pending = None;
            }
        }
        drop(self.http.take());
        self.session.close().await;
    }
}
