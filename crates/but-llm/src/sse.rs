//! Incremental parser for server-sent events (`text/event-stream`).

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SseEvent {
    /// The `event:` field, if the server sent one.
    pub event: Option<String>,
    /// The `data:` lines joined by `\n`.
    pub data: String,
}

/// Turns arbitrary chunks of an event stream into events. Chunk boundaries may fall anywhere, also inside a
/// multi-byte character.
#[derive(Debug, Default)]
pub(crate) struct SseParser {
    pending: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    /// Feed the next chunk and return the events it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.pending.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(newline) = self.pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=newline).collect();
            if let Some(event) = self.line(&line[..line.len() - 1]) {
                events.push(event);
            }
        }
        events
    }

    /// The stream ended: dispatch what a server that closed without a final blank line left behind.
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            if let Some(event) = self.line(&line) {
                return Some(event);
            }
        }
        self.dispatch()
    }

    fn line(&mut self, line: &[u8]) -> Option<SseEvent> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            return self.dispatch();
        }
        if line.starts_with(b":") {
            return None;
        }
        let line = String::from_utf8_lossy(line);
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_ref(), ""),
        };
        match field {
            "data" => self.data.push(value.to_owned()),
            "event" => self.event = Some(value.to_owned()),
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        let event = self.event.take();
        if self.data.is_empty() {
            return None;
        }
        Some(SseEvent {
            event,
            data: std::mem::take(&mut self.data).join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(events: &[SseEvent]) -> Vec<&str> {
        events.iter().map(|e| e.data.as_str()).collect()
    }

    #[test]
    fn events_survive_arbitrary_chunk_boundaries() {
        let stream = "event: a\ndata: one\n\r\ndata: two\ndata: lines\n\n: comment\ndata: ü\n\n";
        let whole = SseParser::default().push(stream.as_bytes());
        for split in 0..stream.len() {
            let mut parser = SseParser::default();
            let mut events = parser.push(&stream.as_bytes()[..split]);
            events.extend(parser.push(&stream.as_bytes()[split..]));
            assert_eq!(
                events, whole,
                "splitting the stream at byte {split} must not change the events"
            );
        }
        assert_eq!(data(&whole), ["one", "two\nlines", "ü"]);
        assert_eq!(whole[0].event.as_deref(), Some("a"));
        assert_eq!(
            whole[1].event, None,
            "the event name does not leak into the next event"
        );
    }

    #[test]
    fn an_unterminated_last_event_is_dispatched_on_finish() {
        let mut parser = SseParser::default();
        assert!(parser.push(b"data: [DONE]").is_empty(), "no newline yet");
        assert_eq!(
            parser.finish().map(|e| e.data),
            Some("[DONE]".to_owned()),
            "a server that closes without a blank line still delivers its last event"
        );
        assert_eq!(parser.finish(), None, "nothing is delivered twice");
    }
}
