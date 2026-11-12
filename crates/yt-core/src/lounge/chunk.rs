//! Browserchannel framing used by `/api/lounge/bc/bind` responses:
//!
//! ```text
//! 270\n
//! [[0,["c","SID","",8]]\n
//! ,[1,["S","gsessionid"]]\n
//! ]\n
//! ```
//!
//! Each chunk is a decimal length line followed by a JSON array of messages `[index, [name, args...]]`.
//! The length counts UTF-16 code units on Google's side (not bytes), so it is unreliable for non-ASCII
//! payloads such as phone names. The parser therefore uses the length line only as a separator and finds
//! the end of each array by bracket matching (string- and escape-aware).

use serde_json::Value;

/// One browserchannel message: `[index, [name, args...]]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub index: i64,
    pub name: String,
    /// Everything after the name (usually zero or one JSON object).
    pub args: Vec<Value>,
}

impl Message {
    pub fn from_value(v: &Value) -> Option<Message> {
        let arr = v.as_array()?;
        let index = arr.first()?.as_i64()?;
        let body = arr.get(1)?.as_array()?;
        let name = body.first()?.as_str()?.to_string();
        Some(Message { index, name, args: body[1..].to_vec() })
    }

    /// The first argument when it is a JSON object (or `Null`).
    pub fn payload(&self) -> &Value {
        static NULL: Value = Value::Null;
        self.args.first().unwrap_or(&NULL)
    }
}

/// Incremental parser: feed raw bytes as they arrive from the long-poll, collect complete messages.
#[derive(Default)]
pub struct ChunkParser {
    buf: Vec<u8>,
}

impl ChunkParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `data` and returns every message from the chunks completed so far.
    pub fn feed(&mut self, data: &[u8]) -> Vec<Message> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            // Skip whitespace and the decimal length line(s) up to the opening bracket.
            let Some(start) = self.buf.iter().position(|&b| b == b'[') else {
                // Keep a pending length prefix; drop anything else (garbage, whitespace).
                if self.buf.iter().any(|b| !b.is_ascii_digit() && !b.is_ascii_whitespace()) {
                    self.buf.clear();
                }
                break;
            };
            let Some(end) = find_array_end(&self.buf[start..]) else { break };
            let slice = &self.buf[start..start + end];
            if let Ok(Value::Array(msgs)) = serde_json::from_slice::<Value>(slice) {
                out.extend(msgs.iter().filter_map(Message::from_value));
            }
            self.buf.drain(..start + end);
        }
        out
    }
}

/// Parses a complete response body (e.g. the initial bind POST) in one go.
pub fn parse_all(body: &[u8]) -> Vec<Message> {
    ChunkParser::new().feed(body)
}

/// Given bytes starting at `[`, returns the length of the balanced top-level array, if complete.
fn find_array_end(b: &[u8]) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    for (i, &c) in b.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIND: &str = "458\n[[0,[\"c\",\"F052B2115F9479B9\",\"\",8]\n]\n,[1,[\"S\",\"S8sDQ9Oxs3ffI3RjBZYcRj0fLOQ7IzbM\"]]\n,[2,[\"loungeStatus\",{\"devices\":\"[{\\\"app\\\":\\\"lb-v4\\\",\\\"name\\\":\\\"YouTube on TV\\\",\\\"type\\\":\\\"LOUNGE_SCREEN\\\"}]\"}]]\n,[3,[\"getNowPlaying\"]]\n]\n";

    #[test]
    fn parses_bind_response() {
        let m = parse_all(BIND.as_bytes());
        assert_eq!(m.len(), 4);
        assert_eq!(m[0].name, "c");
        assert_eq!(m[0].args[0], "F052B2115F9479B9");
        assert_eq!(m[1].name, "S");
        assert_eq!(m[2].name, "loungeStatus");
        assert!(m[2].payload()["devices"].as_str().unwrap().contains("LOUNGE_SCREEN"));
        assert_eq!(m[3].index, 3);
        assert!(m[3].args.is_empty());
    }

    #[test]
    fn incremental_byte_by_byte_and_multiple_chunks() {
        let stream = format!(
            "{BIND}25\n[[4,[\"noop\"]]\n]\n60\n[[5,[\"seekTo\",{{\"newTime\":\"12.5\"}}]]\n,[6,[\"play\"]]\n]\n"
        );
        let mut p = ChunkParser::new();
        let mut all = Vec::new();
        for b in stream.as_bytes() {
            all.extend(p.feed(std::slice::from_ref(b)));
        }
        let names: Vec<_> = all.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["c", "S", "loungeStatus", "getNowPlaying", "noop", "seekTo", "play"]);
        assert_eq!(all[5].payload()["newTime"], "12.5");
    }

    #[test]
    fn wrong_length_prefix_and_brackets_in_strings() {
        // Non-ASCII name, bogus length, brackets and escaped quotes inside strings.
        let s = "3\n[[7,[\"remoteConnected\",{\"name\":\"Телефон [\\\"x\\\"] ]\",\"id\":\"a\"}]]]\n";
        let m = parse_all(s.as_bytes());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].payload()["name"], "Телефон [\"x\"] ]");
    }

    #[test]
    fn partial_chunk_waits() {
        let mut p = ChunkParser::new();
        assert!(p.feed(b"40\n[[1,[\"pause\"]]").is_empty());
        let m = p.feed(b"\n]\n");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "pause");
    }
}
