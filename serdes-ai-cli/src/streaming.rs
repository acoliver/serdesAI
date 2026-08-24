//! Streaming response handler for agent output.
//!
//! Connects agent streaming events to the MessageBus for V2 rendering.

use std::sync::Arc;

use anyhow::Result;
use futures::stream::StreamExt;
use serdes_ai_agent::{AgentStream, AgentStreamEvent};
use tracing::trace;

use crate::bus::{AnyMessage, MessageBus};
use crate::messages::{AgentResponseMessage, BaseMessage, MessageCategory};

/// Handle a streaming agent response.
pub async fn handle_stream(mut stream: AgentStream, bus: Arc<MessageBus>) -> Result<String> {
    let mut full_output = String::new();

    while let Some(event) = stream.next().await {
        match event {
            Ok(AgentStreamEvent::TextDelta { text }) => {
                trace!("Stream text: {}", text);
                full_output.push_str(&text);

                bus.emit(AnyMessage::AgentResponse(AgentResponseMessage {
                    base: BaseMessage::new(MessageCategory::Agent, None),
                    content: full_output.clone(),
                    is_markdown: true,
                    is_streaming: true,
                }));
            }
            Ok(_) => {}
            Err(e) => {
                return Err(anyhow::anyhow!("Stream error: {}", e));
            }
        }
    }

    bus.emit(AnyMessage::AgentResponse(AgentResponseMessage {
        base: BaseMessage::new(MessageCategory::Agent, None),
        content: full_output.clone(),
        is_markdown: true,
        is_streaming: false,
    }));

    Ok(full_output)
}

/// Convert stream to bus messages.
pub fn stream_to_bus(
    stream: AgentStream,
    bus: Arc<MessageBus>,
) -> impl std::future::Future<Output = Result<String>> {
    handle_stream(stream, bus)
}
