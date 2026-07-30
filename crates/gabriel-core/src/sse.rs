//! Server-sent events: the wire format.
//!
//! Streaming endpoints are no longer niche — every LLM API returns one — and a
//! request bench that can only see the first chunk of an event stream is not
//! much use against them.
//!
//! The format looks trivial and isn't: `data` fields accumulate across lines and
//! join with `\n`, a leading space after the colon is stripped but only one,
//! lines without a colon are fields with an empty value, lines starting with a
//! colon are comments, and only a blank line dispatches an event. Following
//! the WHATWG spec here rather than splitting on `\n\n` avoids mangling
//! multi-line payloads.

/// One dispatched event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Event {
    /// The `event:` field, or `None` for the default (`message`).
    pub name: Option<String>,
    /// The `data:` field(s), joined with newlines.
    pub data: String,
    /// The `id:` field, which persists across events until changed.
    pub id: Option<String>,
    /// A `retry:` value in milliseconds, when the server sent one.
    pub retry_ms: Option<u64>,
}

impl Event {
    pub fn is_empty(&self) -> bool {
        self.data.is_empty() && self.name.is_none()
    }

    /// The data parsed as JSON, which is what most APIs send.
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.data).ok()
    }
}

/// Incremental parser: feed it bytes as they arrive, take whole events out.
#[derive(Debug, Default)]
pub struct Parser {
    /// Bytes received but not yet forming a complete line.
    buffer: Vec<u8>,
    /// Fields accumulated for the event being built.
    name: Option<String>,
    data: Vec<String>,
    /// Last id seen. Per spec this persists until the server changes it.
    last_id: Option<String>,
    retry_ms: Option<u64>,
    /// True once a `data`/`event` field has been seen for the current event.
    started: bool,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk and collect any events it completed.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Event> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();

        // Lines end with LF, CRLF, or a lone CR; splitting on LF and trimming a
        // trailing CR covers the first two, and a lone CR is rare enough that
        // treating it as part of the line is the lesser evil versus buffering
        // forever waiting for an LF.
        while let Some(index) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=index).collect();
            let line = &line[..line.len() - 1];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = String::from_utf8_lossy(line).into_owned();
            if let Some(event) = self.line(&line) {
                events.push(event);
            }
        }
        events
    }

    /// Flush an event that the stream ended without a trailing blank line.
    ///
    /// The spec discards it; a developer inspecting a stream that was cut off
    /// wants to see what arrived, so it is returned and the caller can decide.
    pub fn finish(&mut self) -> Option<Event> {
        if !self.buffer.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).into_owned();
            self.line(&line);
        }
        self.started.then(|| self.dispatch())
    }

    fn line(&mut self, line: &str) -> Option<Event> {
        // A blank line dispatches whatever has accumulated.
        if line.is_empty() {
            return self.started.then(|| self.dispatch());
        }
        // A line beginning with a colon is a comment. Servers send these as
        // keep-alives.
        if line.starts_with(':') {
            return None;
        }

        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            // No colon: the whole line is the field name, value empty.
            None => (line, ""),
        };

        match field {
            "event" => {
                self.name = Some(value.to_string());
                self.started = true;
            }
            "data" => {
                self.data.push(value.to_string());
                self.started = true;
            }
            "id" => {
                // A NUL in the id must be ignored per spec.
                if !value.contains('\0') {
                    self.last_id = Some(value.to_string());
                }
                self.started = true;
            }
            "retry" => {
                if let Ok(ms) = value.parse::<u64>() {
                    self.retry_ms = Some(ms);
                }
                self.started = true;
            }
            // Unknown fields are ignored.
            _ => {}
        }
        None
    }

    fn dispatch(&mut self) -> Event {
        let event = Event {
            name: self.name.take(),
            data: self.data.join("\n"),
            id: self.last_id.clone(),
            retry_ms: self.retry_ms.take(),
        };
        self.data.clear();
        self.started = false;
        event
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Vec<Event> {
        let mut parser = Parser::new();
        parser.push(input.as_bytes())
    }

    #[test]
    fn a_blank_line_dispatches_the_event() {
        let events = parse("data: hello\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "hello");
        assert_eq!(events[0].name, None);
    }

    #[test]
    fn nothing_is_dispatched_until_the_blank_line() {
        let mut parser = Parser::new();
        assert!(parser.push(b"data: partial\n").is_empty());
        assert_eq!(parser.push(b"\n").len(), 1);
    }

    #[test]
    fn multiple_data_lines_join_with_newlines() {
        let events = parse("data: line one\ndata: line two\ndata: line three\n\n");
        assert_eq!(events[0].data, "line one\nline two\nline three");
    }

    #[test]
    fn only_one_leading_space_is_stripped() {
        let events = parse("data:  two spaces\n\n");
        assert_eq!(events[0].data, " two spaces");
    }

    #[test]
    fn a_named_event_carries_its_name() {
        let events = parse("event: token\ndata: {\"delta\":\"hi\"}\n\n");
        assert_eq!(events[0].name.as_deref(), Some("token"));
        assert_eq!(events[0].json().unwrap()["delta"], serde_json::json!("hi"));
    }

    #[test]
    fn comments_are_ignored_and_do_not_dispatch() {
        let events = parse(": keep-alive\n: another\n");
        assert!(events.is_empty());
    }

    #[test]
    fn a_keepalive_between_events_does_not_split_them() {
        let events = parse("data: one\n\n: ping\ndata: two\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "one");
        assert_eq!(events[1].data, "two");
    }

    #[test]
    fn crlf_line_endings_work() {
        let events = parse("event: ping\r\ndata: pong\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name.as_deref(), Some("ping"));
        assert_eq!(events[0].data, "pong");
    }

    #[test]
    fn an_id_persists_across_events_until_changed() {
        let events = parse("id: 1\ndata: a\n\ndata: b\n\nid: 2\ndata: c\n\n");
        assert_eq!(events[0].id.as_deref(), Some("1"));
        // The second event never set an id, so it keeps the last one.
        assert_eq!(events[1].id.as_deref(), Some("1"));
        assert_eq!(events[2].id.as_deref(), Some("2"));
    }

    #[test]
    fn a_retry_value_is_captured_and_not_repeated() {
        let events = parse("retry: 5000\ndata: a\n\ndata: b\n\n");
        assert_eq!(events[0].retry_ms, Some(5000));
        assert_eq!(events[1].retry_ms, None);
    }

    #[test]
    fn a_non_numeric_retry_is_ignored() {
        let events = parse("retry: soon\ndata: a\n\n");
        assert_eq!(events[0].retry_ms, None);
    }

    #[test]
    fn a_field_with_no_colon_has_an_empty_value() {
        let events = parse("data\ndata: real\n\n");
        assert_eq!(events[0].data, "\nreal");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let events = parse("foo: bar\ndata: kept\n\n");
        assert_eq!(events[0].data, "kept");
    }

    /// Chunk boundaries fall wherever TCP puts them, including mid-line and
    /// mid-UTF-8. A parser that assumed whole lines per chunk would corrupt this.
    #[test]
    fn events_survive_arbitrary_chunk_boundaries() {
        let stream = "event: token\ndata: caf\u{e9} \u{2615}\n\ndata: second\n\n";
        for split in 1..stream.len() {
            // Only split on character boundaries; a byte-level split of UTF-8 is
            // handled lossily by design, and that is tested separately.
            if !stream.is_char_boundary(split) {
                continue;
            }
            let bytes = stream.as_bytes();
            let mut parser = Parser::new();
            let mut events = parser.push(&bytes[..split]);
            events.extend(parser.push(&bytes[split..]));
            assert_eq!(events.len(), 2, "split at {split} lost an event");
            assert_eq!(events[0].data, "caf\u{e9} \u{2615}", "split at {split} corrupted data");
            assert_eq!(events[1].data, "second");
        }
    }

    #[test]
    fn a_stream_cut_off_mid_event_can_still_be_inspected() {
        let mut parser = Parser::new();
        let events = parser.push(b"event: partial\ndata: arrived");
        assert!(events.is_empty(), "nothing dispatched without a blank line");

        let leftover = parser.finish().expect("the partial event should be recoverable");
        assert_eq!(leftover.name.as_deref(), Some("partial"));
        assert_eq!(leftover.data, "arrived");
    }

    #[test]
    fn finishing_a_clean_stream_yields_nothing_extra() {
        let mut parser = Parser::new();
        parser.push(b"data: complete\n\n");
        assert_eq!(parser.finish(), None);
    }

    #[test]
    fn an_empty_event_is_reported_as_empty() {
        let events = parse("id: 7\n\n");
        assert_eq!(events.len(), 1);
        assert!(events[0].is_empty());
        assert_eq!(events[0].id.as_deref(), Some("7"));
    }
}
