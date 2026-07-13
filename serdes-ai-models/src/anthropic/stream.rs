//! Anthropic SSE stream parser.
//!
//! This module provides streaming support for Anthropic's Messages API.

use super::types::{ContentBlockDelta, ContentBlockStart, StreamEvent};
use crate::error::ModelError;
use bytes::Bytes;
use futures::Stream;
use pin_project_lite::pin_project;
use serdes_ai_core::messages::{
    ModelResponsePartDelta, ModelResponseStreamEvent, PartDeltaEvent, PartEndEvent, PartStartEvent,
    TextPart, ThinkingPart, ThinkingPartDelta, ToolCallPart,
};
use serdes_ai_core::ModelResponsePart;
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

pin_project! {
    /// Anthropic SSE stream parser.
    pub struct AnthropicStreamParser<S> {
        #[pin]
        inner: S,
        buffer: Vec<u8>,
        // Track content blocks in progress
        blocks: HashMap<usize, BlockState>,
        // Message metadata
        message_id: Option<String>,
        model: Option<String>,
        // Usage tracking
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: Option<u64>,
        cache_read_tokens: Option<u64>,
        // Finished
        done: bool,
    }
}

/// State for an in-progress content block.
#[derive(Debug, Clone)]
enum BlockState {
    Text {
        content: String,
    },
    ToolUse {
        #[allow(dead_code)]
        id: String,
        #[allow(dead_code)]
        name: String,
        input_json: String,
    },
    Thinking {
        content: String,
        signature: Option<String>,
    },
    RedactedThinking {
        /// The encrypted signature data.
        #[allow(dead_code)]
        signature: String,
    },
}

impl<S> AnthropicStreamParser<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>>,
{
    /// Create a new stream parser.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: Vec::new(),
            blocks: HashMap::new(),
            message_id: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            done: false,
        }
    }
}

impl<S> Stream for AnthropicStreamParser<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>>,
{
    type Item = Result<ModelResponseStreamEvent, ModelError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        if *this.done {
            return Poll::Ready(None);
        }

        loop {
            // Check for irrecoverably invalid UTF-8 in the buffer before
            // attempting to parse events. Incomplete multibyte sequences at
            // the tail are held until the next chunk arrives.
            if let Err(e) = decode_utf8_prefix(&this.buffer) {
                *this.done = true;
                return Poll::Ready(Some(Err(ModelError::invalid_response(format!(
                    "invalid UTF-8 in stream data: {}",
                    e
                )))));
            }

            // Process complete SSE events from the byte buffer.
            while let Some(event_result) = parse_next_event(&mut this.buffer) {
                match event_result {
                    Ok((event_type, data)) => {
                        if let Some(result) = process_event(
                            &event_type,
                            &data,
                            this.blocks,
                            this.message_id,
                            this.model,
                            this.input_tokens,
                            this.output_tokens,
                            this.cache_creation_tokens,
                            this.cache_read_tokens,
                            this.done,
                        ) {
                            if result.is_err() {
                                *this.done = true;
                            }
                            return Poll::Ready(Some(result));
                        }
                    }
                    Err(e) => {
                        *this.done = true;
                        return Poll::Ready(Some(Err(e)));
                    }
                }
            }

            // Need more data
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    this.buffer.extend_from_slice(&bytes);
                }
                Poll::Ready(Some(Err(e))) => {
                    *this.done = true;
                    return Poll::Ready(Some(Err(ModelError::Other(e.into()))));
                }
                Poll::Ready(None) => {
                    // Inner stream ended. Check for leftover incomplete UTF-8.
                    if let Err(e) = std::str::from_utf8(&this.buffer) {
                        *this.done = true;
                        return Poll::Ready(Some(Err(ModelError::invalid_response(format!(
                            "stream ended with incomplete or invalid UTF-8 in buffer: {}",
                            e
                        )))));
                    }
                    // Check for leftover partial SSE frame
                    if !this.buffer.is_empty() {
                        *this.done = true;
                        return Poll::Ready(Some(Err(ModelError::invalid_response(format!(
                            "stream ended with {} bytes of unparsed SSE data remaining in buffer",
                            this.buffer.len()
                        )))));
                    }
                    *this.done = true;
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Decode the longest valid UTF-8 prefix from the byte buffer.
/// Returns `Ok` if the buffer is valid UTF-8 or ends with an incomplete
/// multibyte sequence (which will be completed by the next chunk).
/// Returns `Err` for irrecoverably invalid UTF-8.
fn decode_utf8_prefix(buffer: &[u8]) -> Result<(), std::str::Utf8Error> {
    match std::str::from_utf8(buffer) {
        Ok(_) => Ok(()),
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            if valid_up_to < buffer.len() {
                let remaining = &buffer[valid_up_to..];
                // If the remaining bytes are a valid (but incomplete) multibyte
                // prefix, wait for more data.
                if is_incomplete_multibyte_prefix(remaining) {
                    return Ok(());
                }
            }
            Err(e)
        }
    }
}

/// Check if the given bytes are a valid (incomplete) prefix of a UTF-8
/// multibyte sequence.
fn is_incomplete_multibyte_prefix(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let first = bytes[0];
    let expected_len = if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else if first >> 3 == 0b11110 {
        4
    } else {
        // Invalid leading byte
        return false;
    };

    // Check that all continuation bytes so far are valid
    for &b in &bytes[1..] {
        if b >> 6 != 0b10 {
            // Not a continuation byte — invalid
            return false;
        }
    }

    bytes.len() < expected_len
}

/// Parse the next complete SSE event from the byte buffer.
///
/// Scans the buffer for an `event:` / `data:` pair terminated by a blank line.
/// On success, drains the consumed bytes from `buffer` and returns the event.
/// Returns `None` if no complete event is found yet.
fn parse_next_event(buffer: &mut Vec<u8>) -> Option<Result<(String, String), ModelError>> {
    let (blank_pos, term_len) = find_blank_line(buffer)?;
    let consume = blank_pos + term_len;

    // Extract the event bytes up to (but not including) the blank line
    let event_bytes = &buffer[..blank_pos];

    let event_str = match std::str::from_utf8(event_bytes) {
        Ok(s) => s,
        Err(e) => {
            buffer.drain(..consume);
            return Some(Err(ModelError::invalid_response(format!(
                "invalid UTF-8 in SSE event: {}",
                e
            ))));
        }
    };

    // Parse event_type and data from the lines
    let mut event_type = None;
    let mut data = None;

    for line in event_str.lines() {
        if let Some(stripped) = line.strip_prefix("event: ") {
            event_type = Some(stripped.to_string());
        } else if let Some(stripped) = line.strip_prefix("data: ") {
            data = Some(stripped.to_string());
        }
    }

    // Drain processed bytes from buffer (in-place, no allocation)
    buffer.drain(..consume);

    match (event_type, data) {
        (Some(et), Some(d)) => Some(Ok((et, d))),
        (None, Some(d)) => Some(Ok(("message".to_string(), d))),
        _ => None,
    }
}

/// Find the position of the first blank line (two consecutive newlines) in
/// the buffer. Returns the byte index of the first newline of the pair, or
/// `None`.
/// Find the first blank line terminator in the buffer. Returns the byte
/// index of the start of the terminator and its length (2 for LF+LF,
/// 4 for CRLF+CRLF, 3 for mixed).
fn find_blank_line(buffer: &[u8]) -> Option<(usize, usize)> {
    for i in 0..buffer.len().saturating_sub(1) {
        // LF LF
        if buffer[i] == 0x0a && buffer[i + 1] == 0x0a {
            return Some((i, 2));
        }
        // CRLF CRLF
        if i + 3 < buffer.len()
            && buffer[i] == 0x0d
            && buffer[i + 1] == 0x0a
            && buffer[i + 2] == 0x0d
            && buffer[i + 3] == 0x0a
        {
            return Some((i, 4));
        }
        // Mixed LF + CRLF
        if i + 2 < buffer.len()
            && buffer[i] == 0x0a
            && buffer[i + 1] == 0x0d
            && buffer[i + 2] == 0x0a
        {
            return Some((i, 3));
        }
    }
    None
}

/// Process a parsed event into a stream event.
#[allow(clippy::too_many_arguments)]
fn process_event(
    _event_type: &str,
    data: &str,
    blocks: &mut HashMap<usize, BlockState>,
    message_id: &mut Option<String>,
    model: &mut Option<String>,
    input_tokens: &mut u64,
    output_tokens: &mut u64,
    cache_creation_tokens: &mut Option<u64>,
    cache_read_tokens: &mut Option<u64>,
    done: &mut bool,
) -> Option<Result<ModelResponseStreamEvent, ModelError>> {
    // Parse the JSON data — reject malformed events rather than silently dropping them
    let event: StreamEvent = match serde_json::from_str(data) {
        Ok(e) => e,
        Err(e) => {
            return Some(Err(ModelError::invalid_response(format!(
                "Failed to parse stream event: {} - data: {}",
                e, data
            ))));
        }
    };

    match event {
        StreamEvent::MessageStart { message } => {
            *message_id = Some(message.id);
            *model = Some(message.model);
            *input_tokens = message.usage.input_tokens;
            *cache_creation_tokens = message.usage.cache_creation_input_tokens;
            *cache_read_tokens = message.usage.cache_read_input_tokens;
            None
        }

        StreamEvent::ContentBlockStart {
            index,
            content_block,
        } => {
            let (state, part) = match content_block {
                ContentBlockStart::Text { text } => (
                    BlockState::Text {
                        content: text.clone(),
                    },
                    ModelResponsePart::Text(TextPart::new(&text)),
                ),
                ContentBlockStart::ToolUse { id, name, input } => (
                    BlockState::ToolUse {
                        id: id.clone(),
                        name: name.clone(),
                        input_json: serde_json::to_string(&input).unwrap_or_default(),
                    },
                    ModelResponsePart::ToolCall(
                        ToolCallPart::new(&name, input).with_tool_call_id(&id),
                    ),
                ),
                ContentBlockStart::Thinking { thinking } => (
                    BlockState::Thinking {
                        content: thinking.clone(),
                        signature: None,
                    },
                    ModelResponsePart::Thinking(ThinkingPart::new(&thinking)),
                ),
                ContentBlockStart::RedactedThinking { data } => (
                    BlockState::RedactedThinking {
                        signature: data.clone(),
                    },
                    ModelResponsePart::Thinking(ThinkingPart::redacted(&data, "anthropic")),
                ),
            };

            blocks.insert(index, state);
            Some(Ok(ModelResponseStreamEvent::PartStart(
                PartStartEvent::new(index, part),
            )))
        }

        StreamEvent::ContentBlockDelta { index, delta } => {
            let state = blocks.get_mut(&index)?;

            match delta {
                ContentBlockDelta::TextDelta { text } => {
                    if let BlockState::Text { content } = state {
                        content.push_str(&text);
                    }
                    Some(Ok(ModelResponseStreamEvent::PartDelta(
                        PartDeltaEvent::text(index, text),
                    )))
                }
                ContentBlockDelta::InputJsonDelta { partial_json } => {
                    if let BlockState::ToolUse { input_json, .. } = state {
                        input_json.push_str(&partial_json);
                    }
                    Some(Ok(ModelResponseStreamEvent::PartDelta(
                        PartDeltaEvent::tool_call_args(index, partial_json),
                    )))
                }
                ContentBlockDelta::ThinkingDelta { thinking } => {
                    if let BlockState::Thinking { content, .. } = state {
                        content.push_str(&thinking);
                    }
                    Some(Ok(ModelResponseStreamEvent::PartDelta(
                        PartDeltaEvent::thinking(index, thinking),
                    )))
                }
                ContentBlockDelta::SignatureDelta { signature } => {
                    if let BlockState::Thinking { signature: sig, .. } = state {
                        match sig {
                            Some(s) => s.push_str(&signature),
                            None => *sig = Some(signature.clone()),
                        }
                    }
                    // Emit signature delta so agents can track it
                    Some(Ok(ModelResponseStreamEvent::PartDelta(PartDeltaEvent {
                        index,
                        delta: ModelResponsePartDelta::Thinking(
                            ThinkingPartDelta::new("").with_signature_delta(signature),
                        ),
                    })))
                }
            }
        }

        StreamEvent::ContentBlockStop { index } => {
            blocks.remove(&index);
            Some(Ok(ModelResponseStreamEvent::PartEnd(PartEndEvent {
                index,
            })))
        }

        StreamEvent::MessageDelta { delta: _, usage } => {
            if let Some(u) = usage {
                *output_tokens = u.output_tokens;
            }
            // We don't emit finish reason as event since core doesn't have it
            None
        }

        StreamEvent::MessageStop => {
            *done = true;
            None
        }

        StreamEvent::Ping => None,

        StreamEvent::Error { error } => Some(Err(ModelError::api_with_code(
            error.message,
            error.error_type,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use futures::StreamExt;

    fn make_sse_bytes(event_type: &str, data: &str) -> Bytes {
        Bytes::from(format!("event: {}\ndata: {}\n\n", event_type, data))
    }

    #[tokio::test]
    async fn test_parse_message_start() {
        let data = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let bytes = vec![Ok(make_sse_bytes("message_start", data))];
        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        // Message start doesn't emit an event
        let event = parser.next().await;
        assert!(event.is_none());
    }

    #[tokio::test]
    async fn test_parse_text_stream() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta)),
            Ok(make_sse_bytes("content_block_stop", block_stop)),
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        // Collect events
        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        // Should have: PartStart, PartDelta, PartEnd
        assert_eq!(events.len(), 3, "Expected 3 events, got {:?}", events);

        assert!(
            matches!(&events[0], ModelResponseStreamEvent::PartStart(_)),
            "First should be PartStart"
        );
        assert!(
            matches!(&events[1], ModelResponseStreamEvent::PartDelta(_)),
            "Second should be PartDelta"
        );
        assert!(
            matches!(&events[2], ModelResponseStreamEvent::PartEnd(_)),
            "Third should be PartEnd"
        );
    }

    #[tokio::test]
    async fn test_parse_tool_use_stream() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start = r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"search","input":{}}}"#;
        let delta1 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#;
        let delta2 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"rust\"}"}}"#;
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta1)),
            Ok(make_sse_bytes("content_block_delta", delta2)),
            Ok(make_sse_bytes("content_block_stop", block_stop)),
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        // Should have: PartStart, PartDelta, PartDelta, PartEnd
        assert_eq!(events.len(), 4, "Expected 4 events, got {:?}", events);

        // First should be PartStart with tool_use
        if let ModelResponseStreamEvent::PartStart(start) = &events[0] {
            assert!(
                matches!(&start.part, ModelResponsePart::ToolCall(_)),
                "Expected ToolCall"
            );
        } else {
            panic!("Expected PartStart");
        }
    }

    #[tokio::test]
    async fn test_parse_error() {
        let error =
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"Rate limited"}}"#;
        let bytes = vec![Ok(make_sse_bytes("error", error))];
        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let event = parser.next().await.unwrap();
        assert!(event.is_err());
        let err = event.unwrap_err();
        assert!(err.to_string().contains("Rate limited"));
    }

    #[tokio::test]
    async fn test_parse_ping() {
        let ping = r#"{"type":"ping"}"#;
        let bytes = vec![Ok(make_sse_bytes("ping", ping))];
        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        // Ping shouldn't emit an event
        let event = parser.next().await;
        assert!(event.is_none());
    }

    // =====================================================
    // Issue #40 regression tests
    // =====================================================

    /// Helper: create a stream that feeds bytes one chunk at a time.
    fn make_byte_chunks(full: &[u8], chunk_size: usize) -> Vec<Result<Bytes, reqwest::Error>> {
        full.chunks(chunk_size)
            .map(|c| Ok(Bytes::copy_from_slice(c)))
            .collect()
    }

    /// Full valid stream as raw bytes (message_start through message_stop).
    fn full_valid_stream_bytes() -> Vec<u8> {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;
        let msg_stop = r#"{"type":"message_stop"}"#;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&make_sse_bytes("message_start", msg_start));
        bytes.extend_from_slice(&make_sse_bytes("content_block_start", block_start));
        bytes.extend_from_slice(&make_sse_bytes("content_block_delta", delta));
        bytes.extend_from_slice(&make_sse_bytes("content_block_stop", block_stop));
        bytes.extend_from_slice(&make_sse_bytes("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#));
        bytes.extend_from_slice(&make_sse_bytes("message_stop", msg_stop));
        bytes
    }

    /// Collect all events from a parser, returning Ok(events) or Err(error).
    async fn collect_events<S>(
        mut parser: AnthropicStreamParser<S>,
    ) -> Result<Vec<ModelResponseStreamEvent>, ModelError>
    where
        S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
    {
        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            match result {
                Ok(e) => events.push(e),
                Err(e) => return Err(e),
            }
        }
        Ok(events)
    }

    #[tokio::test]
    async fn test_byte_fragmentation_all_boundaries() {
        let full = full_valid_stream_bytes();

        for chunk_size in 1..=full.len() {
            let chunks = make_byte_chunks(&full, chunk_size);
            let stream = stream::iter(chunks);
            let parser = AnthropicStreamParser::new(stream);

            let events = collect_events(parser).await.unwrap_or_else(|e| {
                panic!("chunk_size {}: stream failed with error: {}", chunk_size, e);
            });

            assert_eq!(
                events.len(),
                3,
                "chunk_size {}: expected 3 events (PartStart, PartDelta, PartEnd), got {}",
                chunk_size,
                events.len()
            );
        }
    }

    #[tokio::test]
    async fn test_multibyte_utf8_split_at_every_boundary() {
        // Use a multibyte character (U+00E9 = C3 A9) in the text delta
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"caf\u00e9"}}"#;
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;
        let msg_stop = r#"{"type":"message_stop"}"#;

        let full = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&make_sse_bytes("message_start", msg_start));
            bytes.extend_from_slice(&make_sse_bytes("content_block_start", block_start));
            bytes.extend_from_slice(&make_sse_bytes("content_block_delta", delta));
            bytes.extend_from_slice(&make_sse_bytes("content_block_stop", block_stop));
            bytes.extend_from_slice(&make_sse_bytes("message_stop", msg_stop));
            bytes
        };

        // Split at every byte boundary — must reconstruct perfectly
        for split_point in 1..full.len() {
            let chunks = vec![
                Ok(Bytes::copy_from_slice(&full[..split_point])),
                Ok(Bytes::copy_from_slice(&full[split_point..])),
            ];
            let stream = stream::iter(chunks);
            let parser = AnthropicStreamParser::new(stream);

            let events = collect_events(parser).await.unwrap_or_else(|e| {
                panic!(
                    "split_point {}: stream failed with error: {}",
                    split_point, e
                );
            });

            assert_eq!(
                events.len(),
                3,
                "split_point {}: expected 3 events",
                split_point
            );
        }
    }

    #[tokio::test]
    async fn test_malformed_json_yields_typed_error() {
        let bad_data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"broken"#;
        let bytes = vec![Ok(make_sse_bytes("content_block_delta", bad_data))];
        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let event = parser.next().await.unwrap();
        assert!(event.is_err(), "expected error for malformed JSON");
        let err = event.unwrap_err();
        assert!(
            err.to_string().contains("Failed to parse stream event"),
            "error should mention parse failure, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_irrecoverably_invalid_utf8_yields_error() {
        // Build a valid SSE frame but inject invalid UTF-8 byte (0xFF)
        let valid = make_sse_bytes("ping", r#"{"type":"ping"}"#);
        let mut bad_bytes = valid.to_vec();
        // Insert an invalid UTF-8 byte in the data portion
        bad_bytes.insert(20, 0xFF);

        let bytes = vec![Ok(Bytes::from(bad_bytes))];
        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let event = parser.next().await;
        assert!(event.is_some(), "expected an error event for invalid UTF-8");
        if let Some(Err(err)) = event {
            assert!(
                err.to_string().contains("invalid UTF-8"),
                "error should mention invalid UTF-8, got: {}",
                err
            );
        } else {
            panic!("expected Err, got Ok or None");
        }
    }

    #[tokio::test]
    async fn test_malformed_record_between_valid_deltas_no_success() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let delta1 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let bad_data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"broken"#;
        let delta2 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"World"}}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta1)),
            Ok(make_sse_bytes("content_block_delta", bad_data)),
            Ok(make_sse_bytes("content_block_delta", delta2)),
        ];

        let stream = stream::iter(bytes);
        let parser = AnthropicStreamParser::new(stream);

        let result = collect_events(parser).await;

        assert!(
            result.is_err(),
            "stream with malformed JSON between valid deltas must produce an error"
        );

        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("Failed to parse stream event"),
            "error should mention parse failure, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_no_events_after_fatal_parse_error() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let bad_data = r#"not valid json at all"#;
        let delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"should-not-see"}}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_delta", bad_data)),
            Ok(make_sse_bytes("content_block_delta", delta)),
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        // First poll should return the error
        let event = parser.next().await;
        assert!(event.is_some(), "expected error event");
        assert!(
            event.as_ref().unwrap().is_err(),
            "expected Err for malformed JSON"
        );

        // Subsequent polls should return None (parser is done)
        let after = parser.next().await;
        assert!(
            after.is_none(),
            "no events should be emitted after fatal parse error"
        );
    }

    #[tokio::test]
    async fn test_fragmentation_preserves_event_sequence() {
        // Verify that byte-level fragmentation doesn't change the event sequence
        let full = full_valid_stream_bytes();

        // Single chunk (baseline)
        let single = make_byte_chunks(&full, full.len());
        let single_events = collect_events(AnthropicStreamParser::new(stream::iter(single)))
            .await
            .unwrap();

        // Fragmented into 1-byte chunks (worst case)
        let fragmented = make_byte_chunks(&full, 1);
        let frag_events = collect_events(AnthropicStreamParser::new(stream::iter(fragmented)))
            .await
            .unwrap();

        assert_eq!(
            single_events.len(),
            frag_events.len(),
            "fragmented stream should produce same number of events"
        );

        for (i, (s, f)) in single_events.iter().zip(frag_events.iter()).enumerate() {
            assert_eq!(
                std::mem::discriminant(s),
                std::mem::discriminant(f),
                "event {} type mismatch: single={:?} fragmented={:?}",
                i,
                s,
                f
            );
        }
    }

    #[tokio::test]
    async fn test_invalid_utf8_byte_split_across_chunks() {
        // A 3-byte UTF-8 character (U+4E9C = E4 BA 9C) split so byte 1 is in
        // chunk 1 and bytes 2-3 are in chunk 2.
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        // The delta contains the multibyte char embedded in JSON
        let delta_json = "{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"\u{4E9C}\"}}";
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;
        let msg_stop = r#"{"type":"message_stop"}"#;

        let full = {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&make_sse_bytes("message_start", msg_start));
            bytes.extend_from_slice(&make_sse_bytes("content_block_start", block_start));
            bytes.extend_from_slice(&make_sse_bytes("content_block_delta", delta_json));
            bytes.extend_from_slice(&make_sse_bytes("content_block_stop", block_stop));
            bytes.extend_from_slice(&make_sse_bytes("message_stop", msg_stop));
            bytes
        };

        // Find the byte position of the multibyte char in the full stream
        let char_bytes = "\u{4E9C}".as_bytes(); // E4 BA 9C
        let char_pos = full
            .windows(char_bytes.len())
            .position(|w| w == char_bytes)
            .expect("multibyte char should be in stream");

        // Split right in the middle of the multibyte char
        for split_offset in 1..char_bytes.len() {
            let split_point = char_pos + split_offset;
            let chunks = vec![
                Ok(Bytes::copy_from_slice(&full[..split_point])),
                Ok(Bytes::copy_from_slice(&full[split_point..])),
            ];
            let stream = stream::iter(chunks);
            let parser = AnthropicStreamParser::new(stream);

            let events = collect_events(parser).await.unwrap_or_else(|e| {
                panic!("split_offset {}: failed with error: {}", split_offset, e);
            });

            assert_eq!(
                events.len(),
                3,
                "split_offset {}: expected 3 events",
                split_offset
            );
        }
    }
}
