//! Antigravity SSE stream parser.
//!
//! Parses Server-Sent Events from the Antigravity API into model responses.

use super::types::*;
use crate::error::ModelError;
use bytes::Bytes;
use futures::Stream;
use pin_project_lite::pin_project;
use serdes_ai_core::messages::{
    FinishReason, ModelResponseStreamEvent, PartDeltaEvent, PartEndEvent, PartStartEvent,
    StreamCompleteEvent, TextPart, ThinkingPart, ToolCallPart,
};
use serdes_ai_core::ModelResponsePart;
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use tracing::{trace, warn};

pin_project! {
    /// SSE stream parser for Antigravity responses.
    pub struct AntigravityStreamParser<S> {
        #[pin]
        inner: S,
        buffer: String,
        // Track parts in progress
        parts: HashMap<usize, PartState>,
        // Current part index
        next_part_index: usize,
        // Finish reason mapped from the final chunk's candidate finishReason
        finish_reason: Option<FinishReason>,
        // Usage metadata from the wrapped response when present
        usage: Option<UsageMetadata>,
        // Open part indices awaiting a PartEnd before the terminal event
        pending_part_ends: Vec<usize>,
        // Finished: terminal event emitted, stream ended, or stream failed
        done: bool,
    }
}

/// State for an in-progress part.
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum PartState {
    Text { content: String },
    FunctionCall { name: String, args: String },
    Thinking { content: String },
}

impl<S> AntigravityStreamParser<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>>,
{
    /// Create a new stream parser.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: String::new(),
            parts: HashMap::new(),
            next_part_index: 0,
            finish_reason: None,
            usage: None,
            pending_part_ends: Vec::new(),
            done: false,
        }
    }
}

impl<S> Stream for AntigravityStreamParser<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>>,
{
    type Item = Result<ModelResponseStreamEvent, ModelError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        if *this.done {
            return Poll::Ready(None);
        }

        'outer: loop {
            // Finish observed: close every open part, then emit the terminal
            // event exactly once as the stream's last event.
            if let Some(reason) = *this.finish_reason {
                if let Some(idx) = this.pending_part_ends.pop() {
                    return Poll::Ready(Some(Ok(ModelResponseStreamEvent::PartEnd(
                        PartEndEvent { index: idx },
                    ))));
                }
                if !this.parts.is_empty() {
                    // Close parts in ascending index order: pop() takes from
                    // the end, so queue the remaining indices descending.
                    let mut indices: Vec<usize> = this.parts.drain().map(|(idx, _)| idx).collect();
                    indices.sort_unstable();
                    indices.reverse();
                    let first = indices.pop().expect("parts checked non-empty");
                    *this.pending_part_ends = indices;
                    return Poll::Ready(Some(Ok(ModelResponseStreamEvent::PartEnd(
                        PartEndEvent { index: first },
                    ))));
                }

                *this.done = true;
                return Poll::Ready(Some(Ok(stream_complete_event(reason, this.usage.as_ref()))));
            }

            // Try both \r\n\r\n (Windows) and \n\n (Unix) line endings
            let separator = if this.buffer.contains("\r\n\r\n") {
                "\r\n\r\n"
            } else {
                "\n\n"
            };

            // Parse complete SSE events from buffer
            while let Some(event_end) = this.buffer.find(separator) {
                let event = this
                    .buffer
                    .drain(..event_end + separator.len())
                    .collect::<String>();

                // Parse SSE event lines
                for line in event.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        // Handle [DONE] marker
                        if data == "[DONE]" {
                            // Without an observed finishReason the stream is
                            // truncated: end without the terminal event.
                            if this.finish_reason.is_some() {
                                continue 'outer;
                            }
                            *this.done = true;
                            return Poll::Ready(None);
                        }

                        // Parse the JSON response
                        match serde_json::from_str::<AntigravityResponse>(data) {
                            Ok(response) => {
                                // Capture terminal metadata before part events
                                // so it survives final chunks that also carry
                                // content.
                                capture_terminal_metadata(
                                    &response,
                                    this.finish_reason,
                                    this.usage,
                                );
                                if let Some(event) =
                                    process_response(&response, this.parts, this.next_part_index)
                                {
                                    return Poll::Ready(Some(event));
                                }
                                // The finishReason chunk is final: no later
                                // chunk produces events.
                                if this.finish_reason.is_some() {
                                    continue 'outer;
                                }
                            }
                            Err(e) => {
                                warn!("Failed to parse Antigravity stream chunk: {}", e);
                                trace!("Raw data that failed: {}", data);
                            }
                        }
                    }
                }
            }

            // Need more data
            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    if let Ok(text) = std::str::from_utf8(&bytes) {
                        this.buffer.push_str(text);
                    }
                }
                Poll::Ready(Some(Err(e))) => {
                    // Mark done so no event (including the terminal event)
                    // is emitted after an error.
                    *this.done = true;
                    return Poll::Ready(Some(Err(ModelError::Other(e.into()))));
                }
                Poll::Ready(None) => {
                    // Process any remaining buffer without a trailing separator
                    if !this.buffer.is_empty() {
                        let remaining = std::mem::take(this.buffer);
                        for line in remaining.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                if data != "[DONE]" {
                                    if let Ok(response) =
                                        serde_json::from_str::<AntigravityResponse>(data)
                                    {
                                        capture_terminal_metadata(
                                            &response,
                                            this.finish_reason,
                                            this.usage,
                                        );
                                        if let Some(event) = process_response(
                                            &response,
                                            this.parts,
                                            this.next_part_index,
                                        ) {
                                            return Poll::Ready(Some(event));
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if this.finish_reason.is_some() {
                        continue 'outer;
                    }

                    *this.done = true;
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Capture the terminal metadata a chunk carries: a candidate `finishReason`
/// (the final chunk's completion signal) and the wrapped response's
/// `usageMetadata`, so both survive final chunks that also carry content.
fn capture_terminal_metadata(
    response: &AntigravityResponse,
    finish_reason: &mut Option<FinishReason>,
    usage: &mut Option<UsageMetadata>,
) {
    if let Some(reason) = response
        .response
        .candidates
        .first()
        .and_then(|candidate| candidate.finish_reason.as_deref())
    {
        *finish_reason = Some(map_finish_reason(reason));
    }
    if let Some(metadata) = &response.response.usage_metadata {
        *usage = Some(metadata.clone());
    }
}

/// Process a response chunk into stream events.
fn process_response(
    response: &AntigravityResponse,
    parts: &mut HashMap<usize, PartState>,
    next_part_index: &mut usize,
) -> Option<Result<ModelResponseStreamEvent, ModelError>> {
    // Get the first candidate from the wrapped response
    let candidate = response.response.candidates.first()?;
    let content = candidate.content.as_ref()?;

    // Process each part
    for part in &content.parts {
        match part {
            Part::Text { text } => {
                if text.is_empty() {
                    continue;
                }

                // Check if we have an existing text part
                let text_part_idx = parts.iter().find_map(|(idx, state)| {
                    if matches!(state, PartState::Text { .. }) {
                        Some(*idx)
                    } else {
                        None
                    }
                });

                if let Some(idx) = text_part_idx {
                    // Emit delta for existing part
                    if let Some(PartState::Text { content }) = parts.get_mut(&idx) {
                        let delta = text.clone();
                        content.push_str(&delta);
                        return Some(Ok(ModelResponseStreamEvent::PartDelta(
                            PartDeltaEvent::text(idx, delta),
                        )));
                    }
                } else {
                    // Start new text part
                    let idx = *next_part_index;
                    *next_part_index += 1;
                    parts.insert(
                        idx,
                        PartState::Text {
                            content: text.clone(),
                        },
                    );
                    return Some(Ok(ModelResponseStreamEvent::PartStart(
                        PartStartEvent::new(idx, ModelResponsePart::Text(TextPart::new(text))),
                    )));
                }
            }
            Part::FunctionCall {
                function_call,
                thought_signature,
            } => {
                // Start new function call part
                let idx = *next_part_index;
                *next_part_index += 1;

                let mut tool_part =
                    ToolCallPart::new(&function_call.name, function_call.args.clone());
                if let Some(id) = &function_call.id {
                    tool_part = tool_part.with_tool_call_id(id);
                }

                // Store thought signature in provider_details for multi-turn tool calls
                if let Some(sig) = thought_signature {
                    let mut details = serde_json::Map::new();
                    details.insert(
                        "thoughtSignature".to_string(),
                        serde_json::Value::String(sig.clone()),
                    );
                    tool_part.provider_details = Some(details);
                }

                parts.insert(
                    idx,
                    PartState::FunctionCall {
                        name: function_call.name.clone(),
                        args: serde_json::to_string(&function_call.args).unwrap_or_default(),
                    },
                );

                return Some(Ok(ModelResponseStreamEvent::PartStart(
                    PartStartEvent::new(idx, ModelResponsePart::ToolCall(tool_part)),
                )));
            }
            Part::Thinking { thought: _, text } => {
                // thought is a bool flag, text contains the actual thinking content
                if text.is_empty() {
                    continue;
                }

                // Check if we have an existing thinking part
                let think_part_idx = parts.iter().find_map(|(idx, state)| {
                    if matches!(state, PartState::Thinking { .. }) {
                        Some(*idx)
                    } else {
                        None
                    }
                });

                if let Some(idx) = think_part_idx {
                    if let Some(PartState::Thinking { content }) = parts.get_mut(&idx) {
                        let delta = text.clone();
                        content.push_str(&delta);
                        return Some(Ok(ModelResponseStreamEvent::PartDelta(
                            PartDeltaEvent::thinking(idx, delta),
                        )));
                    }
                } else {
                    let idx = *next_part_index;
                    *next_part_index += 1;
                    parts.insert(
                        idx,
                        PartState::Thinking {
                            content: text.clone(),
                        },
                    );
                    return Some(Ok(ModelResponseStreamEvent::PartStart(
                        PartStartEvent::new(
                            idx,
                            ModelResponsePart::Thinking(ThinkingPart::new(text)),
                        ),
                    )));
                }
            }
            Part::ThoughtSignature { .. } => {
                // Thought signatures are used for multi-turn, we can skip them for now
                continue;
            }
            _ => {}
        }
    }

    None
}

/// Build the terminal event from the captured finish reason and usage
/// metadata; the optional cached count maps to `cache_read_tokens` only when
/// reported, and the wire format has no cache-creation count.
fn stream_complete_event(
    finish_reason: FinishReason,
    usage: Option<&UsageMetadata>,
) -> ModelResponseStreamEvent {
    let mut event = StreamCompleteEvent::new(finish_reason);

    if let Some(u) = usage {
        if let Some(tokens) = u.prompt_token_count {
            event = event.with_input_tokens(tokens as u64);
        }
        if let Some(tokens) = u.candidates_token_count {
            event = event.with_output_tokens(tokens as u64);
        }
        if let Some(cached) = u.cached_content_token_count {
            event = event.with_cache_read_tokens(cached as u64);
        }
    }

    ModelResponseStreamEvent::StreamComplete(event)
}

/// Map an Antigravity finish reason string to a [`FinishReason`].
///
/// Unknown reasons map to [`FinishReason::EndTurn`], matching the
/// non-streaming response mapping.
fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "STOP" => FinishReason::EndTurn,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" => FinishReason::ContentFilter,
        _ => FinishReason::EndTurn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use futures::StreamExt;

    fn make_sse_bytes(data: &str) -> Bytes {
        Bytes::from(format!("data: {}\n\n", data))
    }

    /// A final chunk carrying finishReason and usageMetadata ends the stream
    /// with exactly one terminal event, emitted last, mapping the finish
    /// reason and the prompt, candidates, and cached counts.
    #[tokio::test]
    async fn final_chunk_with_finish_and_usage_emits_terminal_stream_complete() {
        let text_chunk = r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}}"#;
        let finish_chunk = r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"text":" World"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15,"cachedContentTokenCount":3}}}"#;

        let bytes = vec![
            Ok(make_sse_bytes(text_chunk)),
            Ok(make_sse_bytes(finish_chunk)),
            // A trailing [DONE] marker must not suppress the terminal event.
            Ok(Bytes::from("data: [DONE]\n\n")),
        ];
        let stream = stream::iter(bytes);
        let mut parser = AntigravityStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        // Content part events precede the part end and the terminal event.
        assert_eq!(
            events.len(),
            4,
            "Expected PartStart, PartDelta, PartEnd, StreamComplete, got {:?}",
            events
        );

        let terminals: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ModelResponseStreamEvent::StreamComplete(_)))
            .collect();
        assert_eq!(terminals.len(), 1, "expected exactly one terminal event");

        match events.last() {
            Some(ModelResponseStreamEvent::StreamComplete(complete)) => {
                assert_eq!(complete.finish_reason, FinishReason::EndTurn);
                assert_eq!(complete.input_tokens, Some(10));
                assert_eq!(complete.output_tokens, Some(5));
                // The wire format reports cached tokens but no cache-creation
                // count.
                assert_eq!(complete.cache_creation_tokens, None);
                assert_eq!(complete.cache_read_tokens, Some(3));
            }
            other => panic!("expected terminal StreamComplete last, got {:?}", other),
        }
    }

    /// A finish chunk without usageMetadata still ends the stream with
    /// exactly one terminal event; every token field stays `None`.
    #[tokio::test]
    async fn finish_without_usage_metadata_yields_none_token_fields() {
        let text_chunk = r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}}"#;
        let finish_chunk = r#"{"response":{"candidates":[{"finishReason":"MAX_TOKENS"}]}}"#;
        let bytes = vec![
            Ok(make_sse_bytes(text_chunk)),
            Ok(make_sse_bytes(finish_chunk)),
        ];
        let stream = stream::iter(bytes);
        let mut parser = AntigravityStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        let terminals: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ModelResponseStreamEvent::StreamComplete(_)))
            .collect();
        assert_eq!(terminals.len(), 1, "expected exactly one terminal event");

        match events.last() {
            Some(ModelResponseStreamEvent::StreamComplete(complete)) => {
                assert_eq!(complete.finish_reason, FinishReason::Length);
                assert_eq!(complete.input_tokens, None);
                assert_eq!(complete.output_tokens, None);
                assert_eq!(complete.cache_creation_tokens, None);
                assert_eq!(complete.cache_read_tokens, None);
            }
            other => panic!("expected terminal StreamComplete last, got {:?}", other),
        }
    }

    /// The terminal event follows every open part's PartEnd when the finish
    /// chunk arrives with multiple parts open.
    #[tokio::test]
    async fn terminal_event_follows_all_open_part_ends() {
        let text_chunk = r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}}"#;
        let tool_chunk = r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"search","args":{"q":"rust"}}}]}}]}}"#;
        let finish_chunk = r#"{"response":{"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":4,"totalTokenCount":12}}}"#;
        let bytes = vec![
            Ok(make_sse_bytes(text_chunk)),
            Ok(make_sse_bytes(tool_chunk)),
            Ok(make_sse_bytes(finish_chunk)),
        ];
        let stream = stream::iter(bytes);
        let mut parser = AntigravityStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        assert_eq!(
            events.len(),
            5,
            "Expected 2 PartStart, 2 PartEnd, StreamComplete, got {:?}",
            events
        );

        match (&events[2], &events[3]) {
            (
                ModelResponseStreamEvent::PartEnd(first),
                ModelResponseStreamEvent::PartEnd(second),
            ) => {
                assert_eq!(first.index, 0, "lowest open part closes first");
                assert_eq!(second.index, 1);
            }
            other => panic!(
                "expected PartEnd events before the terminal, got {:?}",
                other
            ),
        }

        match events.last() {
            Some(ModelResponseStreamEvent::StreamComplete(complete)) => {
                assert_eq!(complete.finish_reason, FinishReason::EndTurn);
                assert_eq!(complete.input_tokens, Some(8));
                assert_eq!(complete.output_tokens, Some(4));
            }
            other => panic!("expected terminal StreamComplete last, got {:?}", other),
        }
    }

    /// A stream that ends without a finish reason emits no terminal event, so
    /// consumers keep treating the truncated stream as incomplete.
    #[tokio::test]
    async fn stream_end_without_finish_reason_emits_no_terminal_event() {
        let text_chunk = r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}}"#;
        let bytes = vec![Ok(make_sse_bytes(text_chunk))];
        let stream = stream::iter(bytes);
        let mut parser = AntigravityStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        assert_eq!(
            events.len(),
            1,
            "expected only the part event, got {:?}",
            events
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ModelResponseStreamEvent::StreamComplete(_))),
            "truncated stream must not emit a terminal event"
        );
    }

    /// A transport error ends the stream with the error; a finish chunk that
    /// arrives after it produces no terminal event.
    #[tokio::test]
    async fn stream_error_suppresses_terminal_event() {
        // reqwest::Error has no public constructor; a refused connection to a
        // closed loopback port is the cheapest real one to obtain.
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let err = client
            .get("http://127.0.0.1:1")
            .send()
            .await
            .expect_err("connection to closed loopback port must fail");

        let text_chunk = r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}}"#;
        let finish_chunk = r#"{"response":{"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15}}}"#;
        let bytes = vec![
            Ok(make_sse_bytes(text_chunk)),
            Err(err),
            Ok(make_sse_bytes(finish_chunk)),
        ];
        let stream = stream::iter(bytes);
        let mut parser = AntigravityStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result);
        }

        assert!(
            events.last().is_some_and(|e| e.is_err()),
            "expected the stream to end with the error, got {:?}",
            events
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Ok(ModelResponseStreamEvent::StreamComplete(_)))),
            "no terminal event may be emitted after an error"
        );
    }

    /// A SAFETY finish reason maps to ContentFilter on the terminal event,
    /// matching the non-streaming response mapping.
    #[tokio::test]
    async fn safety_finish_reason_maps_to_content_filter() {
        let finish_chunk = r#"{"response":{"candidates":[{"finishReason":"SAFETY"}]}}"#;
        let bytes = vec![Ok(make_sse_bytes(finish_chunk))];
        let stream = stream::iter(bytes);
        let mut parser = AntigravityStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        match events.last() {
            Some(ModelResponseStreamEvent::StreamComplete(complete)) => {
                assert_eq!(complete.finish_reason, FinishReason::ContentFilter);
            }
            other => panic!("expected terminal StreamComplete last, got {:?}", other),
        }
    }

    /// A finish chunk arriving without a trailing separator still ends the
    /// stream with the terminal event after the remaining buffer is drained
    /// at EOF.
    #[tokio::test]
    async fn finish_chunk_without_trailing_separator_emits_terminal_stream_complete() {
        let text_chunk = r#"{"response":{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}}"#;
        let finish_chunk = r#"{"response":{"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}}}"#;
        // No trailing separator on the finish line: it is parsed from the
        // remaining buffer at EOF.
        let bytes = vec![
            Ok(make_sse_bytes(text_chunk)),
            Ok(Bytes::from(format!("data: {}", finish_chunk))),
        ];
        let stream = stream::iter(bytes);
        let mut parser = AntigravityStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        assert_eq!(
            events.len(),
            3,
            "Expected PartStart, PartEnd, StreamComplete, got {:?}",
            events
        );

        match events.last() {
            Some(ModelResponseStreamEvent::StreamComplete(complete)) => {
                assert_eq!(complete.finish_reason, FinishReason::EndTurn);
                assert_eq!(complete.input_tokens, Some(7));
                assert_eq!(complete.output_tokens, Some(3));
            }
            other => panic!("expected terminal StreamComplete last, got {:?}", other),
        }
    }
}
