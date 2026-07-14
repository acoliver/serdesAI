//! Server-Sent Events (SSE) parsing.
//!
//! This module provides utilities for parsing SSE streams from HTTP responses.

use crate::error::{StreamError, StreamResult};
use crate::partial_response::ResponseDelta;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use pin_project_lite::pin_project;
use serde::de::DeserializeOwned;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

const MAX_BUFFER_SIZE: usize = 10 * 1024 * 1024;

/// A parsed SSE event.
#[derive(Debug, Clone)]
pub struct SseEvent {
    /// Event type (if specified).
    pub event: Option<String>,
    /// Event data.
    pub data: String,
    /// Event ID (if specified).
    pub id: Option<String>,
    /// Retry timeout (if specified).
    pub retry: Option<u64>,
}

impl SseEvent {
    /// Create a new SSE event with just data.
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            event: None,
            data: data.into(),
            id: None,
            retry: None,
        }
    }

    /// Set the event type.
    pub fn with_event(mut self, event: impl Into<String>) -> Self {
        self.event = Some(event.into());
        self
    }

    /// Set the event ID.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Check if this is a "done" event (e.g., [DONE]).
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]" || self.event.as_deref() == Some("done")
    }

    /// Parse the data as JSON.
    pub fn parse_data<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(&self.data)
    }
}

/// Parser for Server-Sent Events streams.
#[derive(Debug)]
pub struct SseParser {
    buffer: Vec<u8>,
    events: VecDeque<SseEvent>,
    last_event_id: Option<String>,
    max_buffer_size: usize,
}

impl Default for SseParser {
    fn default() -> Self {
        Self {
            buffer: Vec::new(),
            events: VecDeque::new(),
            last_event_id: None,
            max_buffer_size: MAX_BUFFER_SIZE,
        }
    }
}

impl SseParser {
    /// Create a new SSE parser.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum number of unframed bytes retained by the parser.
    #[must_use]
    pub fn with_max_buffer_size(mut self, max_buffer_size: usize) -> Self {
        self.max_buffer_size = max_buffer_size;
        self
    }

    /// Feed bytes into the parser.
    pub fn feed(&mut self, bytes: &Bytes) -> StreamResult<Vec<SseEvent>> {
        self.buffer.extend_from_slice(bytes);
        self.validate_buffer()?;
        self.parse_buffer()
    }

    /// Feed a string into the parser.
    pub fn feed_str(&mut self, s: &str) -> StreamResult<Vec<SseEvent>> {
        self.feed(&Bytes::copy_from_slice(s.as_bytes()))
    }

    /// Validate that EOF occurred at an SSE record boundary.
    pub fn finish(&mut self) -> StreamResult<Vec<SseEvent>> {
        let events = self.parse_buffer()?;
        if self.buffer.is_empty() {
            Ok(events)
        } else {
            self.validate_buffer()?;
            Err(StreamError::IncompleteSse)
        }
    }

    /// Get the next parsed event.
    pub fn next_event(&mut self) -> Option<SseEvent> {
        self.events.pop_front()
    }

    /// Check if there are pending events.
    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    /// Get the last event ID.
    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }

    /// Clear the parser state.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.events.clear();
    }

    fn validate_buffer(&self) -> StreamResult<()> {
        if self.buffer.len() > self.max_buffer_size {
            return Err(StreamError::BufferOverflow);
        }
        if let Err(error) = std::str::from_utf8(&self.buffer) {
            if error.error_len().is_some() {
                return Err(StreamError::InvalidUtf8);
            }
        }
        Ok(())
    }

    fn parse_buffer(&mut self) -> StreamResult<Vec<SseEvent>> {
        let mut parsed_events = Vec::new();

        while let Some((pos, delimiter_len)) = find_event_boundary(&self.buffer) {
            let frame = self.buffer[..pos].to_vec();
            self.buffer.drain(..pos + delimiter_len);
            let event_str = std::str::from_utf8(&frame).map_err(|_| StreamError::InvalidUtf8)?;

            if let Some(event) = self.parse_event(event_str) {
                if let Some(id) = &event.id {
                    self.last_event_id = Some(id.clone());
                }
                self.events.push_back(event.clone());
                parsed_events.push(event);
            }
        }

        self.validate_buffer()?;
        Ok(parsed_events)
    }

    fn parse_event(&self, s: &str) -> Option<SseEvent> {
        let mut event = None;
        let mut data_lines = Vec::new();
        let mut id = None;
        let mut retry = None;
        let normalized = s.replace("\r\n", "\n").replace('\r', "\n");

        for line in normalized.split('\n') {
            if line.is_empty() || line.starts_with(':') {
                continue;
            }

            let (field, value) = match line.split_once(':') {
                Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
                None => (line, ""),
            };

            match field {
                "event" => event = Some(value.to_string()),
                "data" => data_lines.push(value.to_string()),
                "id" if !value.contains('\0') => id = Some(value.to_string()),
                "retry" => retry = value.parse().ok(),
                _ => {}
            }
        }

        if data_lines.is_empty() {
            return None;
        }

        Some(SseEvent {
            event,
            data: data_lines.join("\n"),
            id,
            retry,
        })
    }
}

fn find_event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    const DELIMITERS: [&[u8]; 3] = [b"\r\n\r\n", b"\n\n", b"\r\r"];
    DELIMITERS
        .iter()
        .filter_map(|delimiter| {
            buffer
                .windows(delimiter.len())
                .position(|window| window == *delimiter)
                .map(|position| (position, delimiter.len()))
        })
        .min_by_key(|(position, _)| *position)
}

pin_project! {
    /// Stream adapter that parses SSE from a byte stream.
    pub struct SseStream<S> {
        #[pin]
        inner: S,

        parser: SseParser,
        finished: bool,
    }
}

impl<S> SseStream<S>
where
    S: Stream<Item = Result<Bytes, std::io::Error>>,
{
    /// Create a new SSE stream from a byte stream.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            parser: SseParser::new(),
            finished: false,
        }
    }
}

impl<S> Stream for SseStream<S>
where
    S: Stream<Item = Result<Bytes, std::io::Error>> + Unpin,
{
    type Item = StreamResult<SseEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        // Return buffered events first
        if let Some(event) = this.parser.next_event() {
            return Poll::Ready(Some(Ok(event)));
        }

        if *this.finished {
            return Poll::Ready(None);
        }

        // Poll for more data
        match this.inner.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                if let Err(error) = this.parser.feed(&bytes) {
                    return Poll::Ready(Some(Err(error)));
                }

                if let Some(event) = this.parser.next_event() {
                    Poll::Ready(Some(Ok(event)))
                } else {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(StreamError::Io(e)))),
            Poll::Ready(None) => {
                *this.finished = true;

                if let Err(error) = this.parser.finish() {
                    return Poll::Ready(Some(Err(error)));
                }

                // Return any remaining events
                if let Some(event) = this.parser.next_event() {
                    Poll::Ready(Some(Ok(event)))
                } else {
                    Poll::Ready(None)
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Extension trait for converting SSE events to response deltas.
pub trait SseEventExt {
    /// Convert to a response delta (if possible).
    fn to_response_delta(&self) -> Option<ResponseDelta>;
}

impl SseEventExt for SseEvent {
    fn to_response_delta(&self) -> Option<ResponseDelta> {
        if self.is_done() {
            return Some(ResponseDelta::Finish {
                reason: serdes_ai_core::FinishReason::Stop,
            });
        }

        // Try to parse as JSON and extract delta
        // This is provider-specific, so we attempt common formats
        if let Ok(json) = self.parse_data::<serde_json::Value>() {
            // OpenAI format: choices[0].delta
            if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
                if let Some(choice) = choices.first() {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                            return Some(ResponseDelta::Text {
                                index: 0,
                                content: content.to_string(),
                            });
                        }
                    }
                }
            }

            // Anthropic format: type = "content_block_delta"
            if json.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                if let Some(delta) = json.get("delta") {
                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                        let index =
                            json.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        return Some(ResponseDelta::Text {
                            index,
                            content: text.to_string(),
                        });
                    }
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_parser_basic() {
        let mut parser = SseParser::new();
        parser.feed_str("data: hello\n\n").unwrap();

        let event = parser.next_event().unwrap();
        assert_eq!(event.data, "hello");
        assert!(event.event.is_none());
    }

    #[test]
    fn test_sse_parser_with_event_type() {
        let mut parser = SseParser::new();
        parser.feed_str("event: message\ndata: hello\n\n").unwrap();

        let event = parser.next_event().unwrap();
        assert_eq!(event.event, Some("message".to_string()));
        assert_eq!(event.data, "hello");
    }

    #[test]
    fn test_sse_parser_multiline_data() {
        let mut parser = SseParser::new();
        parser.feed_str("data: line1\ndata: line2\n\n").unwrap();

        let event = parser.next_event().unwrap();
        assert_eq!(event.data, "line1\nline2");
    }

    #[test]
    fn test_sse_parser_multiple_events() {
        let mut parser = SseParser::new();
        parser.feed_str("data: first\n\ndata: second\n\n").unwrap();

        let event1 = parser.next_event().unwrap();
        let event2 = parser.next_event().unwrap();

        assert_eq!(event1.data, "first");
        assert_eq!(event2.data, "second");
        assert!(parser.next_event().is_none());
    }

    #[test]
    fn test_sse_parser_with_id() {
        let mut parser = SseParser::new();
        parser.feed_str("id: 123\ndata: hello\n\n").unwrap();

        let event = parser.next_event().unwrap();
        assert_eq!(event.id, Some("123".to_string()));
        assert_eq!(parser.last_event_id(), Some("123"));
    }

    #[test]
    fn test_sse_parser_with_retry() {
        let mut parser = SseParser::new();
        parser.feed_str("retry: 5000\ndata: hello\n\n").unwrap();

        let event = parser.next_event().unwrap();
        assert_eq!(event.retry, Some(5000));
    }

    #[test]
    fn test_sse_parser_ignores_comments() {
        let mut parser = SseParser::new();
        parser
            .feed_str(": this is a comment\ndata: hello\n\n")
            .unwrap();

        let event = parser.next_event().unwrap();
        assert_eq!(event.data, "hello");
    }

    #[test]
    fn test_sse_event_is_done() {
        let event = SseEvent::data("[DONE]");
        assert!(event.is_done());

        let event = SseEvent::data("hello");
        assert!(!event.is_done());

        let event = SseEvent::data("something").with_event("done");
        assert!(event.is_done());
    }

    #[test]
    fn test_sse_event_parse_data() {
        let event = SseEvent::data("{\"key\": \"value\"}");
        let parsed: serde_json::Value = event.parse_data().unwrap();
        assert_eq!(parsed["key"], "value");
    }

    #[test]
    fn test_sse_parser_incremental() {
        let mut parser = SseParser::new();

        // Feed partial data
        parser.feed_str("data: hel").unwrap();
        assert!(parser.next_event().is_none());

        parser.feed_str("lo\n\n").unwrap();
        let event = parser.next_event().unwrap();
        assert_eq!(event.data, "hello");
    }

    #[test]
    fn test_sse_to_response_delta() {
        let done_event = SseEvent::data("[DONE]");
        let delta = done_event.to_response_delta().unwrap();
        assert!(matches!(delta, ResponseDelta::Finish { .. }));

        let openai_event = SseEvent::data(r#"{"choices":[{"delta":{"content":"Hello"}}]}"#);
        let delta = openai_event.to_response_delta().unwrap();
        if let ResponseDelta::Text { content, .. } = delta {
            assert_eq!(content, "Hello");
        } else {
            panic!("Expected text delta");
        }
    }

    #[test]
    fn strict_framing_corpus() {
        for input in ["data: no-space\n\n", "data:no-space\r\n\r\n"] {
            let mut parser = SseParser::new();
            parser.feed_str(input).unwrap();
            assert_eq!(parser.next_event().unwrap().data, "no-space");
        }

        let mut parser = SseParser::new();
        parser
            .feed_str(": comment\r\nevent: update\r\nid: 7\r\nretry: 100\r\ndata: one\r\ndata:two\r\n\r\n")
            .unwrap();
        let event = parser.next_event().unwrap();
        assert_eq!(event.event.as_deref(), Some("update"));
        assert_eq!(event.id.as_deref(), Some("7"));
        assert_eq!(event.retry, Some(100));
        assert_eq!(event.data, "one\ntwo");

        let mut parser = SseParser::new();
        parser.feed_str(": comment only\n\n").unwrap();
        assert!(parser.next_event().is_none());
    }

    #[test]
    fn framing_is_invariant_at_every_byte_boundary() {
        let input = "event: message\ndata: héllo\n\n".as_bytes();
        for split in 0..=input.len() {
            let mut parser = SseParser::new();
            parser
                .feed(&Bytes::copy_from_slice(&input[..split]))
                .unwrap();
            parser
                .feed(&Bytes::copy_from_slice(&input[split..]))
                .unwrap();
            let event = parser.next_event().unwrap();
            assert_eq!(event.event.as_deref(), Some("message"));
            assert_eq!(event.data, "héllo");
        }
    }

    #[test]
    fn strict_parser_rejects_invalid_utf8_incomplete_eof_and_overflow() {
        let mut parser = SseParser::new();
        assert!(matches!(
            parser.feed(&Bytes::from_static(b"data: \xff\n\n")),
            Err(StreamError::InvalidUtf8)
        ));

        let mut parser = SseParser::new();
        parser.feed_str("data: partial").unwrap();
        assert!(matches!(parser.finish(), Err(StreamError::IncompleteSse)));

        let mut parser = SseParser::new().with_max_buffer_size(4);
        assert!(matches!(
            parser.feed_str("data:"),
            Err(StreamError::BufferOverflow)
        ));
    }
}
