//! Minimal SSE byte-stream splitter for Provider adapters.

/// Incremental SSE parser that yields complete `(event, data)` frames.
pub struct SseParser {
    buffer: Vec<u8>,
    event_name: String,
    data_lines: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buffer: Vec::new(),
            event_name: String::new(),
            data_lines: Vec::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<(String, String)> {
        self.buffer.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(idx) = self.buffer.iter().position(|&b| b == b'\n') {
            // Decode only complete lines so a multi-byte UTF-8 sequence split
            // across chunk boundaries is reassembled before decoding.
            let mut line = String::from_utf8_lossy(&self.buffer[..idx]).into_owned();
            self.buffer.drain(..=idx);
            if line.ends_with('\r') {
                line.pop();
            }
            if line.is_empty() {
                if !self.data_lines.is_empty() || !self.event_name.is_empty() {
                    out.push((
                        std::mem::take(&mut self.event_name),
                        self.data_lines.join("\n"),
                    ));
                    self.data_lines.clear();
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("event:") {
                self.event_name = rest.trim().to_owned();
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data_lines.push(rest.trim_start().to_owned());
            }
        }
        out
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}
