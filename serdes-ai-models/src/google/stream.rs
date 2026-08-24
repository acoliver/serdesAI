//! Google AI SSE stream parser.
//!
//! Google uses a slightly different streaming format - each chunk is a complete
//! JSON response object, not deltas.

use super::types::{GenerateContentResponse, Part, UsageMetadata};
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

pin_project! {
    /// Google AI stream parser.
    pub struct GoogleStreamParser<S> {
        #[pin]
        inner: S,
        buffer: String,
        // Track parts in progress
        parts: HashMap<usize, PartState>,
        // Current part index
        next_part_index: usize,
        // Finish reason mapped from the last seen candidate finishReason
        finish_reason: Option<FinishReason>,
        // Usage metadata from the most recent chunk that reported it
        usage: Option<UsageMetadata>,
        // Open part indices awaiting a PartEnd before the terminal event
        pending_part_ends: Vec<usize>,
        // Finished: terminal event emitted, stream ended, or stream failed
        done: bool,
    }
}

/// State for an in-progress part.
#[derive(Debug, Clone)]
enum PartState {
    Text {
        content: String,
    },
    FunctionCall {
        #[allow(dead_code)]
        name: String,
        #[allow(dead_code)]
        args: String,
    },
    Thinking {
        content: String,
    },
}

impl<S> GoogleStreamParser<S>
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

impl<S> Stream for GoogleStreamParser<S>
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

            // Try to parse complete JSON objects from buffer
            // Google sends each chunk as a complete JSON object on its own line
            while let Some(line_end) = this.buffer.find('\n') {
                let line = this.buffer.drain(..=line_end).collect::<String>();
                let line = line.trim();

                if line.is_empty() {
                    continue;
                }

                // Skip "data: " prefix if present
                let json_str = line.strip_prefix("data: ").unwrap_or(line);

                // Handle [DONE] marker
                if json_str == "[DONE]" {
                    // Without an observed finishReason the stream is
                    // truncated: end without the terminal event.
                    if this.finish_reason.is_some() {
                        continue 'outer;
                    }
                    *this.done = true;
                    return Poll::Ready(None);
                }

                // Parse the JSON response
                match serde_json::from_str::<GenerateContentResponse>(json_str) {
                    Ok(response) => {
                        // Capture terminal metadata before part events so it
                        // survives final chunks that also carry content.
                        capture_terminal_metadata(&response, this.finish_reason, this.usage);
                        if let Some(event) =
                            process_response(&response, this.parts, this.next_part_index)
                        {
                            return Poll::Ready(Some(event));
                        }
                        // The finishReason chunk is final: no later chunk
                        // produces events.
                        if this.finish_reason.is_some() {
                            continue 'outer;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse Google stream chunk: {} - {}", e, json_str);
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
                    // Process any remaining buffer without a trailing newline
                    if !this.buffer.is_empty() {
                        let remaining = std::mem::take(this.buffer);
                        let json_str = remaining.trim();
                        let json_str = json_str.strip_prefix("data: ").unwrap_or(json_str);

                        if !json_str.is_empty() && json_str != "[DONE]" {
                            if let Ok(response) =
                                serde_json::from_str::<GenerateContentResponse>(json_str)
                            {
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
/// (the final chunk's completion signal) and the latest `usageMetadata`, so
/// both survive final chunks that also carry content parts.
fn capture_terminal_metadata(
    response: &GenerateContentResponse,
    finish_reason: &mut Option<FinishReason>,
    usage: &mut Option<UsageMetadata>,
) {
    if let Some(reason) = response
        .candidates
        .first()
        .and_then(|candidate| candidate.finish_reason.as_deref())
    {
        *finish_reason = Some(map_finish_reason(reason));
    }
    if let Some(metadata) = &response.usage_metadata {
        *usage = Some(metadata.clone());
    }
}

/// Process a response chunk into stream events.
fn process_response(
    response: &GenerateContentResponse,
    parts: &mut HashMap<usize, PartState>,
    next_part_index: &mut usize,
) -> Option<Result<ModelResponseStreamEvent, ModelError>> {
    // Get the first candidate
    let candidate = response.candidates.first()?;
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
            Part::FunctionCall { function_call } => {
                // Start new function call part
                let idx = *next_part_index;
                *next_part_index += 1;

                let tool_part = ToolCallPart::new(&function_call.name, function_call.args.clone());

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
            Part::Thought { thought } => {
                if thought.is_empty() {
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
                        let delta = thought.clone();
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
                            content: thought.clone(),
                        },
                    );
                    return Some(Ok(ModelResponseStreamEvent::PartStart(
                        PartStartEvent::new(
                            idx,
                            ModelResponsePart::Thinking(ThinkingPart::new(thought)),
                        ),
                    )));
                }
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
        event = event
            .with_input_tokens(u.prompt_token_count)
            .with_output_tokens(u.candidates_token_count);
        if let Some(cached) = u.cached_content_token_count {
            event = event.with_cache_read_tokens(cached);
        }
    }

    ModelResponseStreamEvent::StreamComplete(event)
}

/// Map a Google finish reason string to a [`FinishReason`].
///
/// Unknown reasons map to [`FinishReason::Stop`], matching the non-streaming
/// response mapping.
fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" | "RECITATION" => FinishReason::ContentFilter,
        "TOOL_CALLS" | "FUNCTION_CALL" => FinishReason::ToolCall,
        _ => FinishReason::Stop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use futures::StreamExt;

    fn make_chunk(json: &str) -> Bytes {
        Bytes::from(format!("{}\n", json))
    }

    #[tokio::test]
    async fn test_parse_text_response() {
        let chunk = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}"#;
        let bytes = vec![Ok(make_chunk(chunk))];
        let stream = stream::iter(bytes);
        let mut parser = GoogleStreamParser::new(stream);

        let event = parser.next().await.unwrap().unwrap();
        assert!(
            matches!(event, ModelResponseStreamEvent::PartStart(_)),
            "Expected PartStart, got {:?}",
            event
        );
    }

    #[tokio::test]
    async fn test_parse_function_call() {
        let chunk = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"search","args":{"q":"rust"}}}]}}]}"#;
        let bytes = vec![Ok(make_chunk(chunk))];
        let stream = stream::iter(bytes);
        let mut parser = GoogleStreamParser::new(stream);

        let event = parser.next().await.unwrap().unwrap();
        if let ModelResponseStreamEvent::PartStart(start) = event {
            assert!(
                matches!(start.part, ModelResponsePart::ToolCall(_)),
                "Expected ToolCall"
            );
        } else {
            panic!("Expected PartStart");
        }
    }

    /// A final chunk with a finish reason but no usageMetadata still ends the
    /// stream with exactly one terminal event; every token field stays None.
    #[tokio::test]
    async fn test_parse_with_finish() {
        let chunk1 = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}"#;
        let chunk2 = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":" World"}]},"finishReason":"STOP"}]}"#;
        let bytes = vec![Ok(make_chunk(chunk1)), Ok(make_chunk(chunk2))];
        let stream = stream::iter(bytes);
        let mut parser = GoogleStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        // Text delta and part end precede the terminal event.
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
                assert_eq!(complete.finish_reason, FinishReason::Stop);
                assert_eq!(complete.input_tokens, None);
                assert_eq!(complete.output_tokens, None);
                assert_eq!(complete.cache_creation_tokens, None);
                assert_eq!(complete.cache_read_tokens, None);
            }
            other => panic!("expected terminal StreamComplete last, got {:?}", other),
        }
    }

    /// A finish chunk arriving without a trailing newline still ends the
    /// stream with the terminal event after the remaining buffer is drained
    /// at EOF.
    #[tokio::test]
    async fn finish_chunk_without_trailing_newline_emits_terminal_stream_complete() {
        let chunk1 = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}"#;
        let chunk2 = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":" World"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15}}"#;
        // No trailing newline on the finish chunk: it is parsed from the
        // remaining buffer at EOF.
        let bytes = vec![Ok(make_chunk(chunk1)), Ok(Bytes::from(chunk2))];
        let stream = stream::iter(bytes);
        let mut parser = GoogleStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        assert_eq!(
            events.len(),
            4,
            "Expected PartStart, PartDelta, PartEnd, StreamComplete, got {:?}",
            events
        );

        match events.last() {
            Some(ModelResponseStreamEvent::StreamComplete(complete)) => {
                assert_eq!(complete.finish_reason, FinishReason::Stop);
                assert_eq!(complete.input_tokens, Some(10));
                assert_eq!(complete.output_tokens, Some(5));
            }
            other => panic!("expected terminal StreamComplete last, got {:?}", other),
        }
    }

    /// A finish-only final chunk without a trailing newline still routes
    /// through the EOF remaining-buffer branch to the terminal event.
    #[tokio::test]
    async fn finish_only_chunk_without_trailing_newline_emits_terminal_stream_complete() {
        let chunk = r#"{"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"totalTokenCount":10}}"#;
        // No trailing newline and no content parts: the EOF branch must
        // route the captured finish reason to the terminal event itself.
        let stream = stream::iter(vec![Ok(Bytes::from(chunk))]);
        let mut parser = GoogleStreamParser::new(stream);

        let mut events = Vec::new();
        while let Some(result) = parser.next().await {
            events.push(result.unwrap());
        }

        assert_eq!(
            events.len(),
            1,
            "expected only the terminal event, got {:?}",
            events
        );

        match events.last() {
            Some(ModelResponseStreamEvent::StreamComplete(complete)) => {
                assert_eq!(complete.finish_reason, FinishReason::Stop);
                assert_eq!(complete.input_tokens, Some(7));
                assert_eq!(complete.output_tokens, Some(3));
            }
            other => panic!("expected terminal StreamComplete last, got {:?}", other),
        }
    }

    /// A final chunk carrying usageMetadata ends the stream with exactly one
    /// terminal event last, mapping prompt, candidates, and cached counts.
    #[tokio::test]
    async fn final_chunk_with_usage_metadata_emits_terminal_stream_complete() {
        let chunk1 = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}"#;
        let chunk2 = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":" World"}]},"finishReason":"MAX_TOKENS"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5,"totalTokenCount":15,"cachedContentTokenCount":3}}"#;
        let bytes = vec![
            Ok(make_chunk(chunk1)),
            Ok(make_chunk(chunk2)),
            // A trailing [DONE] marker must not suppress the terminal event.
            Ok(make_chunk("[DONE]")),
        ];
        let stream = stream::iter(bytes);
        let mut parser = GoogleStreamParser::new(stream);

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
                assert_eq!(complete.finish_reason, FinishReason::Length);
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

    /// The terminal event is sequenced after every open part's PartEnd, in
    /// ascending index order.
    #[tokio::test]
    async fn terminal_event_follows_all_open_part_ends() {
        let text_chunk =
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}"#;
        let tool_chunk = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"search","args":{"q":"rust"}}}]}}]}"#;
        let finish_chunk = r#"{"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":8,"candidatesTokenCount":4,"totalTokenCount":12}}"#;
        let bytes = vec![
            Ok(make_chunk(text_chunk)),
            Ok(make_chunk(tool_chunk)),
            Ok(make_chunk(finish_chunk)),
        ];
        let stream = stream::iter(bytes);
        let mut parser = GoogleStreamParser::new(stream);

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
                assert_eq!(complete.input_tokens, Some(8));
                assert_eq!(complete.output_tokens, Some(4));
                // The optional cached count is absent on the wire: it stays
                // None, never zero.
                assert_eq!(complete.cache_read_tokens, None);
                assert_eq!(complete.cache_creation_tokens, None);
            }
            other => panic!("expected terminal StreamComplete last, got {:?}", other),
        }
    }

    /// Every Google finish reason string maps to the same core variant the
    /// non-streaming path produces, with unknown strings defaulting to Stop.
    #[tokio::test]
    async fn finish_reason_strings_map_to_core_variants() {
        for (wire, expected) in [
            ("STOP", FinishReason::Stop),
            ("MAX_TOKENS", FinishReason::Length),
            ("SAFETY", FinishReason::ContentFilter),
            ("RECITATION", FinishReason::ContentFilter),
            ("TOOL_CALLS", FinishReason::ToolCall),
            ("FUNCTION_CALL", FinishReason::ToolCall),
            ("SOMETHING_NEW", FinishReason::Stop),
        ] {
            let chunk = format!(r#"{{"candidates":[{{"finishReason":"{wire}"}}]}}"#);
            let stream = stream::iter(vec![Ok(make_chunk(&chunk))]);
            let mut parser = GoogleStreamParser::new(stream);

            let mut events = Vec::new();
            while let Some(result) = parser.next().await {
                events.push(result.unwrap());
            }

            match events.last() {
                Some(ModelResponseStreamEvent::StreamComplete(complete)) => {
                    assert_eq!(complete.finish_reason, expected, "wire reason {wire}");
                }
                other => panic!("expected terminal event for {wire}, got {other:?}"),
            }
        }
    }

    /// A stream that ends without a finish reason emits no terminal event, so
    /// consumers keep treating the truncated stream as incomplete.
    #[tokio::test]
    async fn stream_end_without_finish_reason_emits_no_terminal_event() {
        let chunk = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]}}]}"#;
        let bytes = vec![Ok(make_chunk(chunk))];
        let stream = stream::iter(bytes);
        let mut parser = GoogleStreamParser::new(stream);

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
}
