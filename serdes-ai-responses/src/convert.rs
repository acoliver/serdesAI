//! Conversions between Open Responses wire types and serdesAI core types.

use crate::error::ResponsesError;
use crate::types::*;
use base64::Engine as _;
use chrono::Utc;
use serdes_ai_core::messages::{
    BuiltinToolReturnPart, ImageContent, ModelRequest, ModelRequestPart, ModelResponse,
    ModelResponsePart, SystemPromptPart, TextPart, ThinkingPart, ToolCallArgs, ToolCallPart,
    ToolReturnContent, ToolReturnPart, UserContent, UserContentPart, UserPromptPart,
};
use serdes_ai_models::model::ToolChoice;
use serdes_ai_tools::ToolDefinition;
use std::collections::HashMap;

/// Generate a prefixed opaque ID.
#[must_use]
pub fn new_id(prefix: &str) -> String {
    format!("{}{}", prefix, uuid::Uuid::new_v4().simple())
}

/// Convert request tools into serdesAI tool definitions.
///
/// Returns an error for hosted/built-in tool types: this server only brokers
/// client-executed function tools.
pub fn tool_definitions(
    tools: Option<&[ResponsesTool]>,
) -> Result<Vec<ToolDefinition>, ResponsesError> {
    let Some(tools) = tools else {
        return Ok(Vec::new());
    };
    tools
        .iter()
        .map(|tool| match tool {
            ResponsesTool::Function {
                name,
                description,
                parameters,
                strict,
            } => Ok(ToolDefinition {
                name: name.clone(),
                description: description.clone(),
                parameters_json_schema: parameters.clone(),
                strict: *strict,
                outer_typed_dict_key: None,
            }),
            ResponsesTool::Builtin { tool_type } => Err(ResponsesError::InvalidRequest(format!(
                "tool type '{tool_type}' is not supported: this server can only broker client-side function tools"
            ))),
        })
        .collect()
}

/// Map a serdesAI tool definition onto the wire function tool form.
#[must_use]
pub fn tool_to_wire(tool: &ToolDefinition) -> ResponsesTool {
    ResponsesTool::Function {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters_json_schema.clone(),
        strict: tool.strict,
    }
}

/// Map serdesAI conversation history onto `(instructions, input items)` for a
/// client request.
///
/// The first `skip` requests are treated as already delivered on the session
/// and excluded from the input items, which is what makes websocket
/// continuation turns send only the new material. Instructions are always
/// derived from the full history because the Responses API replaces (rather
/// than appends) instructions on chained turns.
pub fn history_to_wire(
    messages: &[ModelRequest],
    skip: usize,
) -> Result<(Option<String>, Vec<InputItem>), ResponsesError> {
    let mut instructions: Vec<String> = Vec::new();
    let mut items = Vec::new();

    for (index, request) in messages.iter().enumerate() {
        for part in &request.parts {
            match part {
                ModelRequestPart::SystemPrompt(system) => {
                    instructions.push(system.content.clone());
                }
                ModelRequestPart::UserPrompt(prompt) if index >= skip => {
                    items.push(InputItem::Easy(EasyInputMessage {
                        role: InputRole::User,
                        content: Some(user_content_to_wire(&prompt.content)?),
                    }));
                }
                ModelRequestPart::ToolReturn(tool_return) if index >= skip => {
                    items.push(function_call_output_item(tool_return));
                }
                ModelRequestPart::BuiltinToolReturn(tool_return) if index >= skip => {
                    items.push(builtin_tool_return_item(tool_return));
                }
                ModelRequestPart::ModelResponse(response) if index >= skip => {
                    items.extend(response_parts_to_items(response));
                }
                ModelRequestPart::RetryPrompt(retry) if index >= skip => {
                    tracing::debug!(tool_call_id = ?retry.tool_call_id, "dropping retry prompt from responses input");
                }
                _ => {}
            }
        }
    }

    let instructions = if instructions.is_empty() {
        None
    } else {
        Some(instructions.join(
            "

",
        ))
    };
    Ok((instructions, items))
}

fn user_content_to_wire(content: &UserContent) -> Result<InputMessageContent, ResponsesError> {
    match content {
        UserContent::Text(text) => Ok(InputMessageContent::Text(text.clone())),
        UserContent::Parts(parts) => {
            let mut converted = Vec::new();
            for part in parts {
                match part {
                    UserContentPart::Text { text } => {
                        converted.push(InputContentPart::InputText { text: text.clone() })
                    }
                    UserContentPart::Image { image } => {
                        let url = match image {
                            ImageContent::Url(url) => url.url.clone(),
                            ImageContent::Binary(binary) => format!(
                                "data:{};base64,{}",
                                binary.media_type,
                                base64::engine::general_purpose::STANDARD.encode(&binary.data)
                            ),
                        };
                        converted.push(InputContentPart::InputImage {
                            image_url: InputImageUrl::Url(url),
                            detail: None,
                        });
                    }
                    UserContentPart::Video { .. }
                    | UserContentPart::Document { .. }
                    | UserContentPart::File { .. } => {
                        return Err(ResponsesError::InvalidRequest(
                            "video/document/file input parts are not supported by the responses protocol client"
                                .to_string(),
                        ));
                    }
                    UserContentPart::Audio { .. } => {
                        return Err(ResponsesError::InvalidRequest(
                            "audio input parts are not supported by the responses protocol client"
                                .to_string(),
                        ));
                    }
                }
            }
            Ok(InputMessageContent::Parts(converted))
        }
    }
}

fn function_call_output_item(tool_return: &ToolReturnPart) -> InputItem {
    let output = match &tool_return.content {
        ToolReturnContent::Text { content } => content.clone(),
        ToolReturnContent::Json { content } => content.to_string(),
        ToolReturnContent::Error { error } => format!("tool error: {}", error.message),
        ToolReturnContent::Multiple { .. } | ToolReturnContent::Image { .. } => {
            "[non-text tool output]".to_string()
        }
    };
    InputItem::Typed(TypedInputItem::FunctionCallOutput {
        call_id: tool_return
            .tool_call_id
            .clone()
            .unwrap_or_else(|| tool_return.tool_name.clone()),
        output,
    })
}

fn builtin_tool_return_item(tool_return: &BuiltinToolReturnPart) -> InputItem {
    InputItem::Typed(TypedInputItem::FunctionCallOutput {
        call_id: tool_return.tool_call_id.clone(),
        output: serde_json::to_string(&tool_return.content).unwrap_or_default(),
    })
}

fn response_parts_to_items(response: &ModelResponse) -> Vec<InputItem> {
    response
        .parts
        .iter()
        .filter_map(|part| match part {
            ModelResponsePart::Text(text) => Some(InputItem::Typed(TypedInputItem::Message {
                role: InputRole::Assistant,
                content: Some(InputMessageContent::Parts(vec![
                    InputContentPart::OutputText {
                        text: text.content.clone(),
                    },
                ])),
            })),
            ModelResponsePart::Thinking(thinking) => {
                let encrypted_content = thinking
                    .provider_details
                    .as_ref()
                    .and_then(|details| details.get("encrypted_content"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                Some(InputItem::Typed(TypedInputItem::Reasoning {
                    id: None,
                    summary: vec![SummaryTextItem::new(thinking.content.clone())],
                    encrypted_content,
                }))
            }
            ModelResponsePart::ToolCall(call) => {
                Some(InputItem::Typed(TypedInputItem::FunctionCall {
                    call_id: call
                        .tool_call_id
                        .clone()
                        .unwrap_or_else(|| call.tool_name.clone()),
                    name: call.tool_name.clone(),
                    arguments: call
                        .args
                        .to_json_string()
                        .unwrap_or_else(|_| "{}".to_string()),
                }))
            }
            ModelResponsePart::File(_) | ModelResponsePart::BuiltinToolCall(_) => None,
        })
        .collect()
}

/// Map completed output items back onto serdesAI response parts (used by the
/// non-streaming HTTP path).
#[must_use]
pub fn parts_from_output(output: &[OutputItem]) -> Vec<ModelResponsePart> {
    output
        .iter()
        .map(|item| match item {
            OutputItem::Message { content, .. } => {
                let text = content
                    .iter()
                    .map(|part| match part {
                        OutputContent::OutputText { text, .. } => text.clone(),
                        OutputContent::Refusal { refusal } => refusal.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("");
                ModelResponsePart::Text(TextPart::new(text))
            }
            OutputItem::Reasoning { summary, .. } => {
                let text = summary
                    .iter()
                    .map(|part| part.text.as_str())
                    .collect::<Vec<_>>()
                    .join(
                        "
",
                    );
                ModelResponsePart::Thinking(ThinkingPart::new(text))
            }
            OutputItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => ModelResponsePart::ToolCall(ToolCallPart {
                tool_name: name.clone(),
                args: ToolCallArgs::from(arguments.as_str()),
                tool_call_id: Some(call_id.clone()),
                id: None,
                provider_details: None,
            }),
        })
        .collect()
}

/// Map a serdesAI tool choice onto the wire tool choice.
#[must_use]
pub fn tool_choice_to_wire(choice: Option<&ToolChoice>) -> Option<ResponsesToolChoice> {
    let choice = choice?;
    Some(match choice {
        ToolChoice::Auto => ResponsesToolChoice::Mode(ToolChoiceMode::Auto),
        ToolChoice::None => ResponsesToolChoice::Mode(ToolChoiceMode::None),
        ToolChoice::Required => ResponsesToolChoice::Mode(ToolChoiceMode::Required),
        ToolChoice::Specific(name) => ResponsesToolChoice::Function {
            kind: ToolChoiceFunctionTag,
            function: ToolChoiceFunction { name: name.clone() },
        },
    })
}

/// Map an Open Responses tool choice onto the serdesAI model tool choice.
#[must_use]
pub fn tool_choice(choice: Option<&ResponsesToolChoice>) -> Option<ToolChoice> {
    match choice {
        None => None,
        Some(ResponsesToolChoice::Mode(mode)) => Some(match mode {
            ToolChoiceMode::Auto => ToolChoice::Auto,
            ToolChoiceMode::None => ToolChoice::None,
            ToolChoiceMode::Required => ToolChoice::Required,
        }),
        Some(ResponsesToolChoice::Function { function, .. }) => {
            Some(ToolChoice::Specific(function.name.clone()))
        }
    }
}

/// Convert the request input into serdesAI conversation history.
///
/// Instructions become a leading system prompt part. Consecutive assistant
/// items (messages, reasoning, function calls) are folded into a single
/// [`ModelResponse`] request part, mirroring how serdesAI agents record
/// runs; consecutive function call outputs are folded into a single request
/// carrying tool return parts.
pub fn input_to_history(
    input: &ResponseInput,
    instructions: Option<&str>,
    stored_history: &[ModelRequest],
) -> Result<Vec<ModelRequest>, ResponsesError> {
    let mut history: Vec<ModelRequest> = Vec::new();
    if let Some(instructions) = instructions {
        let mut request = ModelRequest::new();
        request.add_part(ModelRequestPart::SystemPrompt(SystemPromptPart::new(
            instructions,
        )));
        history.push(request);
    }

    // call_id -> tool name, seeded from function calls already present in
    // stored history (so a chained turn's function_call_output resolves a
    // call made in an earlier turn), then extended by function_call items in
    // input order.
    let mut call_names: HashMap<String, String> = stored_history
        .iter()
        .flat_map(|request| request.parts.iter())
        .filter_map(|part| match part {
            ModelRequestPart::ModelResponse(response) => Some(response.parts.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|part| match part {
            ModelResponsePart::ToolCall(call) => call
                .tool_call_id
                .clone()
                .map(|call_id| (call_id, call.tool_name.clone())),
            _ => None,
        })
        .collect();

    enum Pending {
        Assistant(Vec<ModelResponsePart>),
        ToolReturns(Vec<ModelRequestPart>),
    }
    let mut pending: Option<Pending> = None;

    let flush_assistant = |history: &mut Vec<ModelRequest>, parts: &mut Option<Pending>| {
        if let Some(Pending::Assistant(parts)) = parts.take() {
            if !parts.is_empty() {
                let response = ModelResponse {
                    parts,
                    model_name: None,
                    timestamp: Utc::now(),
                    finish_reason: None,
                    usage: None,
                    vendor_id: None,
                    vendor_details: None,
                    kind: "response".to_string(),
                };
                let mut request = ModelRequest::new();
                request.add_part(ModelRequestPart::ModelResponse(Box::new(response)));
                history.push(request);
            }
        }
    };

    let flush_tool_returns = |history: &mut Vec<ModelRequest>, parts: &mut Option<Pending>| {
        if let Some(Pending::ToolReturns(parts)) = parts.take() {
            if !parts.is_empty() {
                history.push(ModelRequest::with_parts(parts));
            }
        }
    };

    let items: Vec<InputItem> = match input {
        ResponseInput::Text(text) => vec![InputItem::Easy(EasyInputMessage {
            role: InputRole::User,
            content: Some(InputMessageContent::Text(text.clone())),
        })],
        ResponseInput::Items(items) => items.clone(),
    };

    for item in items {
        match item {
            InputItem::Easy(EasyInputMessage { role, content }) => {
                flush_assistant(&mut history, &mut pending);
                flush_tool_returns(&mut history, &mut pending);
                push_message_role(&mut history, role, content.as_ref())?;
            }
            InputItem::Typed(TypedInputItem::Message { role, content }) => {
                flush_assistant(&mut history, &mut pending);
                flush_tool_returns(&mut history, &mut pending);
                push_message_role(&mut history, role, content.as_ref())?;
            }
            InputItem::Typed(TypedInputItem::FunctionCall {
                call_id,
                name,
                arguments,
            }) => {
                flush_tool_returns(&mut history, &mut pending);
                call_names.insert(call_id.clone(), name.clone());
                let part = match &mut pending {
                    Some(Pending::Assistant(parts)) => parts,
                    _ => {
                        flush_assistant(&mut history, &mut pending);
                        pending = Some(Pending::Assistant(Vec::new()));
                        match &mut pending {
                            Some(Pending::Assistant(parts)) => parts,
                            _ => unreachable!("just set to Assistant"),
                        }
                    }
                };
                part.push(ModelResponsePart::ToolCall(ToolCallPart {
                    tool_name: name,
                    args: ToolCallArgs::from(arguments),
                    tool_call_id: Some(call_id),
                    id: None,
                    provider_details: None,
                }));
            }
            InputItem::Typed(TypedInputItem::FunctionCallOutput { call_id, output }) => {
                flush_assistant(&mut history, &mut pending);
                let tool_name = call_names
                    .get(&call_id)
                    .cloned()
                    .unwrap_or_else(|| call_id.clone());
                let part = match &mut pending {
                    Some(Pending::ToolReturns(parts)) => parts,
                    _ => {
                        flush_tool_returns(&mut history, &mut pending);
                        pending = Some(Pending::ToolReturns(Vec::new()));
                        match &mut pending {
                            Some(Pending::ToolReturns(parts)) => parts,
                            _ => unreachable!("just set to ToolReturns"),
                        }
                    }
                };
                part.push(ModelRequestPart::ToolReturn(ToolReturnPart {
                    tool_name,
                    content: output_content(&output),
                    tool_call_id: Some(call_id),
                    timestamp: Utc::now(),
                }));
            }
            InputItem::Typed(TypedInputItem::Reasoning {
                summary,
                encrypted_content,
                ..
            }) => {
                flush_tool_returns(&mut history, &mut pending);
                let text = summary
                    .iter()
                    .map(|item| item.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let part = match &mut pending {
                    Some(Pending::Assistant(parts)) => parts,
                    _ => {
                        flush_assistant(&mut history, &mut pending);
                        pending = Some(Pending::Assistant(Vec::new()));
                        match &mut pending {
                            Some(Pending::Assistant(parts)) => parts,
                            _ => unreachable!("just set to Assistant"),
                        }
                    }
                };
                let mut thinking = ThinkingPart::new(text);
                if let Some(encrypted) = encrypted_content {
                    thinking = thinking.with_provider_details(
                        [(
                            "encrypted_content".to_string(),
                            serde_json::Value::String(encrypted),
                        )]
                        .into_iter()
                        .collect(),
                    );
                }
                part.push(ModelResponsePart::Thinking(thinking));
            }
            InputItem::Typed(TypedInputItem::ItemReference { id }) => {
                return Err(ResponsesError::InvalidRequest(format!(
                    "item_reference '{id}' cannot be resolved: item references require server-side item storage"
                )));
            }
        }
    }
    flush_assistant(&mut history, &mut pending);
    flush_tool_returns(&mut history, &mut pending);
    Ok(history)
}

fn push_message_role(
    history: &mut Vec<ModelRequest>,
    role: InputRole,
    content: Option<&InputMessageContent>,
) -> Result<(), ResponsesError> {
    let content = content
        .cloned()
        .unwrap_or(InputMessageContent::Text(String::new()));
    match role {
        InputRole::User => {
            let mut request = ModelRequest::new();
            request.add_part(ModelRequestPart::UserPrompt(UserPromptPart::new(
                user_content(&content)?,
            )));
            history.push(request);
        }
        InputRole::Assistant => {
            let text = match &content {
                InputMessageContent::Text(text) => text.clone(),
                InputMessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|part| match part {
                        InputContentPart::InputText { text }
                        | InputContentPart::OutputText { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            };
            let mut request = ModelRequest::new();
            let mut response = ModelResponse::new();
            response
                .parts
                .push(ModelResponsePart::Text(TextPart::new(text)));
            request.add_part(ModelRequestPart::ModelResponse(Box::new(response)));
            history.push(request);
        }
        InputRole::System | InputRole::Developer => {
            let text = match &content {
                InputMessageContent::Text(text) => text.clone(),
                InputMessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|part| match part {
                        InputContentPart::InputText { text }
                        | InputContentPart::OutputText { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""),
            };
            let mut request = ModelRequest::new();
            request.add_part(ModelRequestPart::SystemPrompt(SystemPromptPart::new(text)));
            history.push(request);
        }
    }
    Ok(())
}

fn user_content(content: &InputMessageContent) -> Result<UserContent, ResponsesError> {
    match content {
        InputMessageContent::Text(text) => Ok(UserContent::Text(text.clone())),
        InputMessageContent::Parts(parts) => {
            let mut converted = Vec::new();
            for part in parts {
                match part {
                    InputContentPart::InputText { text }
                    | InputContentPart::OutputText { text } => {
                        converted.push(UserContentPart::Text { text: text.clone() });
                    }
                    InputContentPart::InputImage { image_url, .. } => {
                        converted.push(UserContentPart::Image {
                            image: ImageContent::url(image_url.url().to_string()),
                        });
                    }
                    InputContentPart::InputAudio { .. } => {
                        return Err(ResponsesError::InvalidRequest(
                            "input_audio parts are not supported by this server".to_string(),
                        ));
                    }
                }
            }
            Ok(UserContent::Parts(converted))
        }
    }
}

fn output_content(output: &str) -> ToolReturnContent {
    match serde_json::from_str::<serde_json::Value>(output) {
        Ok(value @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) => {
            ToolReturnContent::Json { content: value }
        }
        _ => ToolReturnContent::Text {
            content: output.to_string(),
        },
    }
}

/// Build output items from a completed [`ModelResponse`].
#[must_use]
pub fn output_items_from_response(response: &ModelResponse) -> Vec<OutputItem> {
    response
        .parts
        .iter()
        .filter_map(|part| match part {
            ModelResponsePart::Text(text) => Some(OutputItem::Message {
                id: new_id("msg_"),
                role: "assistant".to_string(),
                status: OutputItemStatus::Completed,
                content: vec![OutputContent::OutputText {
                    text: text.content.clone(),
                    annotations: Vec::new(),
                }],
            }),
            ModelResponsePart::Thinking(thinking) => Some(OutputItem::Reasoning {
                id: new_id("rs_"),
                summary: vec![SummaryTextItem::new(thinking.content.clone())],
                encrypted_content: None,
            }),
            ModelResponsePart::ToolCall(call) => Some(OutputItem::FunctionCall {
                id: new_id("fc_"),
                call_id: call.tool_call_id.clone().unwrap_or_else(|| new_id("call_")),
                name: call.tool_name.clone(),
                arguments: call
                    .args
                    .to_json_string()
                    .unwrap_or_else(|_| "{}".to_string()),
                status: OutputItemStatus::Completed,
            }),
            ModelResponsePart::File(_) | ModelResponsePart::BuiltinToolCall(_) => None,
        })
        .collect()
}
