//! Anthropic SSE stream parser.
//!
//! This module provides streaming support for Anthropic's Messages API.

use super::types::{ContentBlockDelta, ContentBlockStart, StreamEvent};
use crate::error::ModelError;
use bytes::Bytes;
use futures::Stream;
use pin_project_lite::pin_project;
use serdes_ai_core::messages::{
    FinishReason, ModelResponsePartDelta, ModelResponseStreamEvent, PartDeltaEvent, PartEndEvent,
    PartStartEvent, StreamCompleteEvent, TextPart, ThinkingPart, ThinkingPartDelta, ToolCallPart,
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
        buffer: String,
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
        // Provider-reported stop reason from message_delta
        stop_reason: Option<String>,
        // Whether message_stop was observed
        message_stop_seen: bool,
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
            buffer: String::new(),
            blocks: HashMap::new(),
            message_id: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            stop_reason: None,
            message_stop_seen: false,
            done: false,
        }
    }

    /// The provider-reported stop reason (from `message_delta`), if any.
    ///
    /// This is only reliable after the stream has completed successfully
    /// (i.e. `message_stop` was observed).
    pub fn stop_reason(&self) -> Option<&str> {
        self.stop_reason.as_deref()
    }

    /// Whether `message_stop` was observed before the stream ended.
    pub fn message_stop_seen(&self) -> bool {
        self.message_stop_seen
    }

    /// Input tokens reported by the provider.
    pub fn input_tokens(&self) -> u64 {
        self.input_tokens
    }

    /// Output tokens reported by the provider.
    pub fn output_tokens(&self) -> u64 {
        self.output_tokens
    }
}

impl<S> Stream for AnthropicStreamParser<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>>,
{
    type Item = Result<ModelResponseStreamEvent, ModelError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        // After StreamComplete has been emitted the stream is done.
        if *this.done {
            return Poll::Ready(None);
        }

        loop {
            // Check if we have complete events in the buffer
            // Anthropic uses "event: type\ndata: json\n\n" format
            while let Some(event_result) = parse_next_event(this.buffer) {
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
                            this.stop_reason,
                            this.message_stop_seen,
                            this.done,
                        ) {
                            // If the result is StreamComplete, mark done so the
                            // next poll returns None without re-checking EOF.
                            if let Ok(ModelResponseStreamEvent::StreamComplete(_)) = &result {
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

            // If message_stop was already seen and all buffered events consumed,
            // we should not poll the inner stream further — just end.
            if *this.message_stop_seen {
                *this.done = true;
                return Poll::Ready(None);
            }

            // Need more data
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        this.buffer.push_str(text);
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    *this.done = true;
                    return Poll::Ready(Some(Err(ModelError::Other(e.into()))));
                }
                Poll::Ready(None) => {
                    // Inner stream reached EOF. If we haven't seen message_stop,
                    // this is a premature EOF — emit a typed error.
                    if !*this.message_stop_seen {
                        *this.done = true;
                        return Poll::Ready(Some(Err(ModelError::incomplete_stream(
                            "stream ended before message_stop was received",
                        ))));
                    }
                    // message_stop was seen but done wasn't set (shouldn't happen
                    // since StreamComplete sets done, but guard anyway).
                    *this.done = true;
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Parse the next complete event from the buffer.
/// Returns Some((event_type, data)) if a complete event was found.
fn parse_next_event(buffer: &mut String) -> Option<Result<(String, String), ModelError>> {
    // Look for event: and data: lines followed by blank line
    let mut event_type = None;
    let mut data = None;
    let mut end_pos = 0;
    let mut found_event = false;

    for (i, line) in buffer.lines().enumerate() {
        if let Some(stripped) = line.strip_prefix("event: ") {
            event_type = Some(stripped.to_string());
        } else if let Some(stripped) = line.strip_prefix("data: ") {
            data = Some(stripped.to_string());
        } else if line.is_empty() && (event_type.is_some() || data.is_some()) {
            // Calculate position after this empty line
            end_pos = buffer
                .lines()
                .take(i + 1)
                .map(|l| l.len() + 1) // +1 for newline
                .sum();
            found_event = true;
            break;
        }
    }

    if found_event {
        // Remove processed content from buffer
        buffer.drain(..end_pos.min(buffer.len()));

        match (event_type, data) {
            (Some(et), Some(d)) => return Some(Ok((et, d))),
            (None, Some(d)) => return Some(Ok(("message".to_string(), d))),
            _ => {}
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
    stop_reason: &mut Option<String>,
    message_stop_seen: &mut bool,
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
                        // If the accumulated JSON is still just the initial empty
                        // object, replace it with the first real delta.
                        if input_json == "{}" {
                            input_json.clear();
                            input_json.push_str(&partial_json);
                        } else {
                            input_json.push_str(&partial_json);
                        }
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
            // Validate tool input JSON before accepting the block as complete.
            // Skip validation when the input is the default empty object (no
            // deltas were received).
            if let Some(BlockState::ToolUse { input_json, .. }) = blocks.get(&index) {
                if !input_json.is_empty() && input_json != "{}" {
                    if serde_json::from_str::<serde_json::Value>(input_json).is_err() {
                        return Some(Err(ModelError::invalid_response(format!(
                            "content_block_stop for tool at index {} has incomplete JSON input: {}",
                            index, input_json
                        ))));
                    }
                }
            }
            blocks.remove(&index);
            Some(Ok(ModelResponseStreamEvent::PartEnd(PartEndEvent {
                index,
            })))
        }

        StreamEvent::MessageDelta { delta, usage } => {
            if let Some(reason) = &delta.stop_reason {
                *stop_reason = Some(reason.clone());
            }
            if let Some(u) = usage {
                *output_tokens = u.output_tokens;
            }
            None
        }

        StreamEvent::MessageStop => {
            // Atomically validate terminal state before accepting message_stop.
            if !blocks.is_empty() {
                let open_indices: Vec<usize> = blocks.keys().copied().collect();
                *done = true;
                return Some(Err(ModelError::incomplete_stream(format!(
                    "message_stop received with open content block(s) at index/indices: {:?}",
                    open_indices
                ))));
            }

            *message_stop_seen = true;

            // Map the Anthropic stop reason to FinishReason
            let finish_reason = map_stop_reason(stop_reason.as_deref());

            // Emit StreamComplete with provider terminal metadata
            let mut event = StreamCompleteEvent::new(finish_reason);
            event = event
                .with_input_tokens(*input_tokens)
                .with_output_tokens(*output_tokens);

            Some(Ok(ModelResponseStreamEvent::StreamComplete(event)))
        }

        StreamEvent::Ping => None,

        StreamEvent::Error { error } => Some(Err(ModelError::api_with_code(
            error.message,
            error.error_type,
        ))),
    }
}

/// Map an Anthropic stop reason string to a [`FinishReason`].
fn map_stop_reason(reason: Option<&str>) -> FinishReason {
    match reason {
        Some("end_turn") => FinishReason::EndTurn,
        Some("stop_sequence") => FinishReason::StopSequence,
        Some("max_tokens") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCall,
        _ => FinishReason::Stop,
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
        let msg_stop = r#"{"type":"message_stop"}"#;
        let bytes = vec![
            Ok(make_sse_bytes("message_start", data)),
            Ok(make_sse_bytes("message_stop", msg_stop)),
        ];
        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        // Message start emits nothing; message_stop emits StreamComplete
        let event = parser.next().await;
        assert!(event.is_some());
        assert!(
            matches!(
                event.unwrap().unwrap(),
                ModelResponseStreamEvent::StreamComplete(_)
            ),
            "expected StreamComplete"
        );
        // Next poll should be None (done)
        assert!(parser.next().await.is_none());
    }

    #[tokio::test]
    async fn test_parse_text_stream() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;
        let msg_delta = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#;
        let msg_stop = r#"{"type":"message_stop"}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta)),
            Ok(make_sse_bytes("content_block_stop", block_stop)),
            Ok(make_sse_bytes("message_delta", msg_delta)),
            Ok(make_sse_bytes("message_stop", msg_stop)),
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        // Collect events
        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        // Should have: PartStart, PartDelta, PartEnd, StreamComplete
        assert_eq!(events.len(), 4, "Expected 4 events, got {:?}", events);

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
        assert!(
            matches!(&events[3], ModelResponseStreamEvent::StreamComplete(_)),
            "Fourth should be StreamComplete"
        );

        // Provider terminal metadata should survive
        assert_eq!(parser.stop_reason(), Some("end_turn"));
        assert!(parser.message_stop_seen());
        assert_eq!(parser.output_tokens(), 5);
    }

    #[tokio::test]
    async fn test_parse_tool_use_stream() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start = r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"search","input":{}}}"#;
        let delta1 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#;
        let delta2 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"rust\"}"}}"#;
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;
        let msg_stop = r#"{"type":"message_stop"}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta1)),
            Ok(make_sse_bytes("content_block_delta", delta2)),
            Ok(make_sse_bytes("content_block_stop", block_stop)),
            Ok(make_sse_bytes("message_stop", msg_stop)),
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        // Should have: PartStart, PartDelta, PartDelta, PartEnd, StreamComplete
        assert_eq!(events.len(), 5, "Expected 5 events, got {:?}", events);

        // First should be PartStart with tool_use
        if let ModelResponseStreamEvent::PartStart(start) = &events[0] {
            assert!(
                matches!(&start.part, ModelResponsePart::ToolCall(_)),
                "Expected ToolCall"
            );
        } else {
            panic!("Expected PartStart");
        }
        // Last should be StreamComplete
        assert!(
            matches!(&events[4], ModelResponseStreamEvent::StreamComplete(_)),
            "Last should be StreamComplete"
        );
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
        let msg_stop = r#"{"type":"message_stop"}"#;
        let bytes = vec![
            Ok(make_sse_bytes("ping", ping)),
            Ok(make_sse_bytes("message_stop", msg_stop)),
        ];
        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        // Ping emits nothing; message_stop emits StreamComplete
        let event = parser.next().await;
        assert!(event.is_some());
        assert!(
            matches!(
                event.unwrap().unwrap(),
                ModelResponseStreamEvent::StreamComplete(_)
            ),
            "expected StreamComplete"
        );
        // Done
        assert!(parser.next().await.is_none());
    }

    // ========================================================================
    // Regression tests for issue #39: premature EOF must not be treated as
    // successful completion.
    // ========================================================================

    /// Helper: build a partial (incomplete) SSE frame as bytes.
    fn make_partial_sse_bytes(event_type: &str, partial_data: &str) -> Bytes {
        // Intentionally missing the trailing blank line so the frame is incomplete
        Bytes::from(format!(
            "event: {}
data: {}",
            event_type, partial_data
        ))
    }

    /// Test 1: EOF before any `message_stop`.
    #[tokio::test]
    async fn test_eof_before_message_stop() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let delta =
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#;
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta)),
            Ok(make_sse_bytes("content_block_stop", block_stop)),
            // NO message_delta, NO message_stop — stream just ends
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let mut had_error = false;
        while let Some(result) = parser.next().await {
            if let Err(ref e) = result {
                assert!(
                    matches!(e, ModelError::IncompleteStream(_)),
                    "expected IncompleteStream, got: {:?}",
                    e
                );
                had_error = true;
            }
        }
        assert!(had_error, "expected an IncompleteStream error");
        assert!(!parser.message_stop_seen());
    }

    /// Test 2: EOF with a partial final SSE record in the buffer.
    #[tokio::test]
    async fn test_eof_with_partial_sse_frame() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let delta =
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#;
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta)),
            Ok(make_sse_bytes("content_block_stop", block_stop)),
            // A partial frame that will never be completed — inner stream ends
            Ok(make_partial_sse_bytes(
                "message_delta",
                r#"{"type":"message_delta","#,
            )),
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let mut had_error = false;
        while let Some(result) = parser.next().await {
            if let Err(ref e) = result {
                assert!(
                    matches!(e, ModelError::IncompleteStream(_)),
                    "expected IncompleteStream for partial frame, got: {:?}",
                    e
                );
                had_error = true;
            }
        }
        assert!(
            had_error,
            "expected IncompleteStream error for partial SSE frame"
        );
    }

    /// Test 3: EOF with an open text block.
    #[tokio::test]
    async fn test_eof_with_open_text_block() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        // NO content_block_stop, NO message_stop

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta)),
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let mut had_error = false;
        while let Some(result) = parser.next().await {
            if let Err(ref e) = result {
                assert!(
                    matches!(e, ModelError::IncompleteStream(_)),
                    "expected IncompleteStream for open text block, got: {:?}",
                    e
                );
                had_error = true;
            }
        }
        assert!(
            had_error,
            "expected IncompleteStream error for open text block"
        );
    }

    /// Test 4: EOF with an open thinking block.
    #[tokio::test]
    async fn test_eof_with_open_thinking_block() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start = r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#;
        let delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Hmm"}}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta)),
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let mut had_error = false;
        while let Some(result) = parser.next().await {
            if let Err(ref e) = result {
                assert!(
                    matches!(e, ModelError::IncompleteStream(_)),
                    "expected IncompleteStream for open thinking block, got: {:?}",
                    e
                );
                had_error = true;
            }
        }
        assert!(
            had_error,
            "expected IncompleteStream error for open thinking block"
        );
    }

    /// Test 5: EOF with an open tool-use block (incomplete tool JSON).
    #[tokio::test]
    async fn test_eof_with_open_tool_use_block() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start = r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"search","input":{}}}"#;
        let delta1 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}"#;
        // Second delta makes the JSON incomplete (missing closing brace)
        let delta2 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"rust\""}}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta1)),
            Ok(make_sse_bytes("content_block_delta", delta2)),
            // NO content_block_stop, NO message_stop
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let mut had_error = false;
        while let Some(result) = parser.next().await {
            if let Err(ref e) = result {
                assert!(
                    matches!(e, ModelError::IncompleteStream(_)),
                    "expected IncompleteStream for open tool block, got: {:?}",
                    e
                );
                had_error = true;
            }
        }
        assert!(
            had_error,
            "expected IncompleteStream error for open tool-use block"
        );
    }

    /// Test 6: `content_block_stop` followed by EOF without `message_stop`.
    #[tokio::test]
    async fn test_content_block_stop_then_eof_without_message_stop() {
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
            // block is closed but no message_stop
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let mut had_error = false;
        while let Some(result) = parser.next().await {
            if let Err(ref e) = result {
                assert!(
                    matches!(e, ModelError::IncompleteStream(_)),
                    "expected IncompleteStream without message_stop, got: {:?}",
                    e
                );
                had_error = true;
            }
        }
        assert!(had_error, "expected IncompleteStream error");
        assert!(!parser.message_stop_seen());
    }

    /// Test 7: A valid sequence ending in `message_stop` produces no error.
    #[tokio::test]
    async fn test_valid_stream_with_message_stop() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let delta = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let block_stop = r#"{"type":"content_block_stop","index":0}"#;
        let msg_delta = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#;
        let msg_stop = r#"{"type":"message_stop"}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta)),
            Ok(make_sse_bytes("content_block_stop", block_stop)),
            Ok(make_sse_bytes("message_delta", msg_delta)),
            Ok(make_sse_bytes("message_stop", msg_stop)),
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let mut saw_stream_complete = false;
        while let Some(result) = parser.next().await {
            match result {
                Ok(ModelResponseStreamEvent::StreamComplete(sc)) => {
                    saw_stream_complete = true;
                    assert_eq!(sc.finish_reason, FinishReason::EndTurn);
                    assert_eq!(sc.input_tokens, Some(10));
                    assert_eq!(sc.output_tokens, Some(5));
                }
                Ok(_) => {}
                Err(e) => panic!("valid stream should not produce error: {:?}", e),
            }
        }
        assert!(
            saw_stream_complete,
            "valid stream should emit StreamComplete"
        );
        assert!(parser.message_stop_seen());
        assert_eq!(parser.stop_reason(), Some("end_turn"));
        assert_eq!(parser.output_tokens(), 5);
        assert_eq!(parser.input_tokens(), 10);
    }

    /// Test 8: Premature EOF after visible text produces an error and no
    /// further successful events.
    #[tokio::test]
    async fn test_premature_eof_after_text_produces_error() {
        let msg_start = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-3-5-sonnet-20241022","usage":{"input_tokens":10,"output_tokens":0}}}"#;
        let block_start =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let delta1 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let delta2 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}"#;

        let bytes = vec![
            Ok(make_sse_bytes("message_start", msg_start)),
            Ok(make_sse_bytes("content_block_start", block_start)),
            Ok(make_sse_bytes("content_block_delta", delta1)),
            Ok(make_sse_bytes("content_block_delta", delta2)),
            // Stream ends here — open text block, no message_stop
        ];

        let stream = stream::iter(bytes);
        let mut parser = AnthropicStreamParser::new(stream);

        let mut text_deltas_seen = 0;
        let mut error_seen = false;
        while let Some(result) = parser.next().await {
            match result {
                Ok(ModelResponseStreamEvent::PartDelta(PartDeltaEvent {
                    delta: ModelResponsePartDelta::Text(_),
                    ..
                })) => {
                    text_deltas_seen += 1;
                }
                Ok(_) => {}
                Err(e) => {
                    assert!(
                        matches!(e, ModelError::IncompleteStream(_)),
                        "expected IncompleteStream, got: {:?}",
                        e
                    );
                    error_seen = true;
                }
            }
        }
        assert!(text_deltas_seen >= 2, "should have seen text deltas");
        assert!(error_seen, "must see IncompleteStream error");
        assert!(!parser.message_stop_seen());
    }
}
