//! Mapping of wire stream events onto serdesAI model stream events.
//!
//! The wire protocol is verbose (lifecycle events for every part); serdesAI
//! only cares about part starts, deltas, part ends, and exactly one terminal
//! `StreamComplete` carrying the finish reason and usage. Terminal integrity
//! follows the crate-wide contract: the terminal event is always last, and a
//! failed turn surfaces as an error instead of a synthetic completion.

use crate::types::{OutputItem, ResponseObject, StreamEvent};
use serdes_ai_core::messages::{
    ModelResponsePart, ModelResponsePartDelta, ModelResponseStreamEvent, PartDeltaEvent,
    PartEndEvent, PartStartEvent, StreamCompleteEvent, TextPart, TextPartDelta, ThinkingPart,
    ThinkingPartDelta, ToolCallArgs, ToolCallPart, ToolCallPartDelta,
};
use serdes_ai_core::FinishReason;
use serdes_ai_models::ModelError;

/// Translate one wire event into zero or more model stream events.
pub(super) fn translate(event: StreamEvent) -> Vec<Result<ModelResponseStreamEvent, ModelError>> {
    match event {
        StreamEvent::ResponseCreated { .. } | StreamEvent::ResponseInProgress { .. } => Vec::new(),

        StreamEvent::OutputItemAdded {
            output_index, item, ..
        } => {
            vec![Ok(ModelResponseStreamEvent::PartStart(
                PartStartEvent::new(output_index as usize, part_from_item(&item)),
            ))]
        }

        StreamEvent::OutputItemDone { output_index, .. } => {
            vec![Ok(ModelResponseStreamEvent::PartEnd(PartEndEvent::new(
                output_index as usize,
            )))]
        }

        StreamEvent::ContentPartAdded { .. } | StreamEvent::ContentPartDone { .. } => Vec::new(),

        StreamEvent::OutputTextDelta {
            output_index,
            delta,
            ..
        } => vec![Ok(ModelResponseStreamEvent::PartDelta(
            PartDeltaEvent::new(
                output_index as usize,
                ModelResponsePartDelta::Text(TextPartDelta {
                    content_delta: delta,
                    provider_details: None,
                }),
            ),
        ))],

        StreamEvent::OutputTextDone { .. } => Vec::new(),

        StreamEvent::ReasoningSummaryPartAdded { .. }
        | StreamEvent::ReasoningSummaryPartDone { .. } => Vec::new(),

        StreamEvent::ReasoningSummaryTextDelta {
            output_index,
            delta,
            ..
        } => vec![Ok(ModelResponseStreamEvent::PartDelta(
            PartDeltaEvent::new(
                output_index as usize,
                ModelResponsePartDelta::Thinking(ThinkingPartDelta {
                    content_delta: delta,
                    signature_delta: None,
                    provider_name: None,
                    provider_details: None,
                }),
            ),
        ))],

        StreamEvent::ReasoningSummaryTextDone { .. } => Vec::new(),

        StreamEvent::FunctionCallArgumentsDelta {
            output_index,
            delta,
            ..
        } => vec![Ok(ModelResponseStreamEvent::PartDelta(
            PartDeltaEvent::new(
                output_index as usize,
                ModelResponsePartDelta::ToolCall(ToolCallPartDelta {
                    args_delta: delta,
                    tool_call_id: None,
                    provider_details: None,
                }),
            ),
        ))],

        StreamEvent::FunctionCallArgumentsDone { .. } => Vec::new(),

        StreamEvent::ResponseCompleted { response, .. } => {
            vec![Ok(ModelResponseStreamEvent::StreamComplete(
                stream_complete(&response, FinishReason::Stop),
            ))]
        }

        StreamEvent::ResponseIncomplete { response, .. } => {
            vec![Ok(ModelResponseStreamEvent::StreamComplete(
                stream_complete(&response, FinishReason::Length),
            ))]
        }

        StreamEvent::ResponseFailed { response, .. } => vec![Err(failure(&response))],
    }
}

/// The initial part for an output item.
pub(super) fn part_from_item(item: &OutputItem) -> ModelResponsePart {
    match item {
        OutputItem::Message { .. } => ModelResponsePart::Text(TextPart::new("")),
        OutputItem::Reasoning { .. } => ModelResponsePart::Thinking(ThinkingPart::new("")),
        OutputItem::FunctionCall { name, call_id, .. } => {
            ModelResponsePart::ToolCall(ToolCallPart {
                tool_name: name.clone(),
                args: ToolCallArgs::String(String::new()),
                tool_call_id: Some(call_id.clone()),
                id: None,
                provider_details: None,
            })
        }
    }
}

/// Build the terminal event with usage mapped from the response object.
pub(super) fn stream_complete(
    response: &ResponseObject,
    reason: FinishReason,
) -> StreamCompleteEvent {
    let (input_tokens, output_tokens) = match &response.usage {
        Some(usage) => (usage.input_tokens, usage.output_tokens),
        None => (None, None),
    };
    StreamCompleteEvent {
        finish_reason: reason,
        input_tokens,
        output_tokens,
        cache_creation_tokens: None,
        cache_read_tokens: None,
    }
}

/// Map a `response.failed` object onto a model error.
pub(super) fn failure(response: &ResponseObject) -> ModelError {
    let body = response.error.as_ref();
    let code = body
        .map(|error| error.code.clone())
        .unwrap_or_else(|| "response_failed".to_string());
    ModelError::provider(
        "open-responses",
        code.clone(),
        body.map(|error| error.message.clone())
            .unwrap_or_else(|| "response failed".to_string()),
        super::failure_kind(&code),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{OutputContent, OutputItemStatus, ResponseUsage};

    #[test]
    fn item_add_starts_parts() {
        let message = OutputItem::Message {
            id: "msg_1".into(),
            role: "assistant".to_string(),
            status: OutputItemStatus::InProgress,
            content: vec![OutputContent::OutputText {
                text: String::new(),
                annotations: Vec::new(),
            }],
        };
        let events = translate(StreamEvent::OutputItemAdded {
            sequence_number: 1,
            output_index: 0,
            item: message.clone(),
        });
        assert!(matches!(
            events[0],
            Ok(ModelResponseStreamEvent::PartStart(_))
        ));

        let call = OutputItem::FunctionCall {
            id: "fc_1".into(),
            call_id: "call_1".into(),
            name: "get_weather".into(),
            arguments: String::new(),
            status: OutputItemStatus::InProgress,
        };
        let events = translate(StreamEvent::OutputItemAdded {
            sequence_number: 2,
            output_index: 1,
            item: call,
        });
        match &events[0] {
            Ok(ModelResponseStreamEvent::PartStart(start)) => {
                assert!(matches!(
                    start.part,
                    ModelResponsePart::ToolCall(ref call)
                        if call.tool_name == "get_weather" && call.tool_call_id.as_deref() == Some("call_1")
                ));
            }
            other => panic!("expected part start, got {other:?}"),
        }
    }

    #[test]
    fn completed_maps_usage_and_finish_reason() {
        let response = ResponseObject {
            id: "resp_1".into(),
            usage: Some(ResponseUsage {
                input_tokens: Some(7),
                output_tokens: Some(4),
                total_tokens: Some(11),
            }),
            ..response_fixture()
        };
        let events = translate(StreamEvent::ResponseCompleted {
            sequence_number: 9,
            response,
        });
        match &events[0] {
            Ok(ModelResponseStreamEvent::StreamComplete(complete)) => {
                assert_eq!(complete.finish_reason, FinishReason::Stop);
                assert_eq!(complete.input_tokens, Some(7));
                assert_eq!(complete.output_tokens, Some(4));
            }
            other => panic!("expected stream complete, got {other:?}"),
        }
    }

    fn response_fixture() -> ResponseObject {
        use crate::types::CreateResponseRequest;
        ResponseObject::in_progress(
            "resp_1",
            0,
            "gpt-4o",
            &CreateResponseRequest {
                model: "gpt-4o".into(),
                input: crate::types::ResponseInput::Text(String::new()),
                instructions: None,
                tools: None,
                tool_choice: None,
                temperature: None,
                top_p: None,
                max_output_tokens: None,
                stream: None,
                background: None,
                store: None,
                previous_response_id: None,
                reasoning: None,
                parallel_tool_calls: None,
                metadata: None,
                user: None,
                truncation: None,
                include: None,
                text: None,
                service_tier: None,
            },
        )
    }
}
