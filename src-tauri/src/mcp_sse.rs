//! Incremental SSE framing, including CR, LF, CRLF, split UTF-8 and an optional
//! initial BOM. Incomplete trailing events never become JSON-RPC responses.
#[derive(Default)]
pub(super) struct Decoder {
    line: Vec<u8>,
    data: Vec<u8>,
    skip_lf: bool,
    started: bool,
    limit: usize,
}
impl Decoder {
    pub fn bounded(limit: usize) -> Result<Self, String> {
        if limit == 0 || limit > super::protocol::LIMIT {
            return Err("MCP_SSE_FRAME_LIMIT".into());
        }
        Ok(Self {
            limit,
            ..Self::default()
        })
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>, String> {
        let mut events = vec![];
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\r' || byte == b'\n' {
                self.skip_lf = byte == b'\r';
                let line =
                    String::from_utf8(std::mem::take(&mut self.line)).map_err(|_| "MCP_UTF8")?;
                let line = if !self.started {
                    line.trim_start_matches('\u{feff}')
                } else {
                    &line
                };
                self.started = true;
                if line.is_empty() {
                    if !self.data.is_empty() {
                        self.data.pop(); // The final data separator is not part of the event.
                        let data = String::from_utf8(std::mem::take(&mut self.data))
                            .map_err(|_| "MCP_UTF8")?;
                        if !data.is_empty() {
                            if events.len() >= 256 {
                                return Err("MCP_NOTIFICATION_LIMIT".into());
                            }
                            events.push(data);
                        }
                    }
                } else {
                    let (field, value) = line.split_once(':').unwrap_or((line, ""));
                    if field == "data" {
                        self.data
                            .extend_from_slice(value.strip_prefix(' ').unwrap_or(value).as_bytes());
                        self.data.push(b'\n');
                    }
                }
            } else {
                self.line.push(byte);
            }
            if self.line.len().saturating_add(self.data.len())
                > if self.limit == 0 {
                    super::protocol::LIMIT
                } else {
                    self.limit
                }
            {
                return Err("HTTP_RESPONSE_LIMIT".into());
            }
        }
        Ok(events)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixed_framing_split_utf8_bom_comments_and_multiple_data_lines() {
        let bytes = "\u{feff}:keepalive\r\nid: opaque\rretry: 1\ndata: {\r\ndata: \"text\":\"España\"}\n\r\ndata:\n\ndata: trailing".as_bytes();
        let mut d = Decoder::default();
        let mut events = vec![];
        for byte in bytes {
            events.extend(d.push(&[*byte]).unwrap());
        }
        assert_eq!(events, ["{\n\"text\":\"España\"}"]);
        // No delimiter: trailing data is not dispatched at EOF.
        assert!(d.push(&[]).unwrap().is_empty());
    }
    #[test]
    fn non_utf8_and_unbounded_events_are_rejected() {
        assert_eq!(
            Decoder::default()
                .push(&[b'd', b'a', b't', b'a', b':', 0xff, b'\n'])
                .unwrap_err(),
            "MCP_UTF8"
        );
        assert_eq!(
            Decoder::default()
                .push(&b"data: {}\n\n".repeat(257))
                .unwrap_err(),
            "MCP_NOTIFICATION_LIMIT"
        );
    }
}
