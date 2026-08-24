//! How a turn presents itself.
//!
//! A turn has a shape: it opens, it works while the user waits, it may show what
//! the model was thinking, it answers, and it closes with what it cost. Each of
//! those was either missing or improvised — the spinner wrote escape codes
//! straight to stderr behind the renderer's back, and nothing reported reasoning
//! or token usage at all, despite the renderer having displays for both.
//!
//! Putting it here means one place decides what a turn looks like, and the
//! renderer owns the drawing.

use std::sync::Arc;
use std::time::Instant;

use serdes_ai_agent::AgentRunResult;
use serdes_ai_core::ModelResponsePart;

use crate::bus::{AnyMessage, MessageBus};
use crate::messages::{
    AgentReasoningMessage, AgentResponseMessage, BaseMessage, DividerMessage, MessageCategory,
    SpinnerAction, SpinnerControl, StatusPanelMessage, StatusType,
};

/// A turn in progress.
///
/// Created when work starts and consumed when it finishes, so the spinner cannot
/// be left running by an early return.
pub struct Turn {
    bus: Arc<MessageBus>,
    spinner_id: String,
    started: Instant,
}

impl Turn {
    /// Open a turn: rule off the previous one and start the waiting indicator.
    pub fn begin(bus: Arc<MessageBus>) -> Self {
        let spinner_id = uuid::Uuid::new_v4().to_string();

        // A rule between turns is what makes a long session readable: without it
        // one exchange runs into the next with no visual boundary.
        bus.emit(AnyMessage::Divider(DividerMessage {
            base: BaseMessage::new(MessageCategory::Divider, None),
            title: None,
        }));

        bus.emit(AnyMessage::SpinnerControl(SpinnerControl {
            base: BaseMessage::new(MessageCategory::System, None),
            spinner_id: spinner_id.clone(),
            action: SpinnerAction::Start,
            message: Some("Thinking".to_string()),
        }));

        Self {
            bus,
            spinner_id,
            started: Instant::now(),
        }
    }

    /// Change what the user is told is happening.
    pub fn progress(&self, message: impl Into<String>) {
        self.bus.emit(AnyMessage::SpinnerControl(SpinnerControl {
            base: BaseMessage::new(MessageCategory::System, None),
            spinner_id: self.spinner_id.clone(),
            action: SpinnerAction::Update,
            message: Some(message.into()),
        }));
    }

    /// Close the turn: reasoning, then the answer, then what it cost.
    ///
    /// The answer is emitted here rather than by the caller so the order is
    /// fixed in one place. Emitting it afterwards put the cost summary above the
    /// answer it described.
    pub fn finish(self, result: &AgentRunResult<String>) {
        self.stop_spinner();

        // Reasoning comes before the answer: it is how the model got there.
        if let Some(reasoning) = extract_reasoning(result) {
            self.bus
                .emit(AnyMessage::AgentReasoning(AgentReasoningMessage {
                    base: BaseMessage::new(MessageCategory::Agent, None),
                    reasoning,
                }));
        }

        self.bus
            .emit(AnyMessage::AgentResponse(AgentResponseMessage {
                base: BaseMessage::new(MessageCategory::Agent, None),
                content: result.output.clone(),
                is_markdown: true,
                is_streaming: false,
            }));

        self.bus.emit(AnyMessage::StatusPanel(StatusPanelMessage {
            base: BaseMessage::new(MessageCategory::System, None),
            title: "Turn".to_string(),
            content: summarise(result, self.started.elapsed()),
            status_type: StatusType::Info,
        }));
    }

    /// Close the turn after a failure.
    ///
    /// The spinner has to stop either way: a turn that errored while the
    /// indicator kept spinning would look like it was still working.
    pub fn fail(self, error: &str) {
        self.stop_spinner();

        self.bus.emit(AnyMessage::StatusPanel(StatusPanelMessage {
            base: BaseMessage::new(MessageCategory::System, None),
            title: "Turn failed".to_string(),
            content: format!("{error}\nAfter {}", format_duration(self.started.elapsed())),
            status_type: StatusType::Error,
        }));
    }

    fn stop_spinner(&self) {
        self.bus.emit(AnyMessage::SpinnerControl(SpinnerControl {
            base: BaseMessage::new(MessageCategory::System, None),
            spinner_id: self.spinner_id.clone(),
            action: SpinnerAction::Stop,
            message: None,
        }));
    }
}

/// The model's reasoning across the turn, if it produced any.
///
/// Only thinking-capable models return this, so most turns have none and show
/// nothing rather than an empty panel.
fn extract_reasoning(result: &AgentRunResult<String>) -> Option<String> {
    let mut parts = Vec::new();

    for response in &result.responses {
        for part in &response.parts {
            if let ModelResponsePart::Thinking(thinking) = part {
                let text = thinking.content.trim();
                if !text.is_empty() {
                    parts.push(text.to_string());
                }
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// What the turn cost, for the closing panel.
fn summarise(result: &AgentRunResult<String>, elapsed: std::time::Duration) -> String {
    let usage = &result.usage;
    let mut lines = Vec::new();

    // Providers do not all report usage. Saying "not reported" is honest;
    // printing zeros would look like a turn that cost nothing.
    if usage.total_tokens > 0 {
        lines.push(format!(
            "{} tokens  ({} in, {} out)",
            usage.total_tokens, usage.request_tokens, usage.response_tokens
        ));
    } else {
        lines.push("tokens not reported by this provider".to_string());
    }

    if let Some(cached) = usage.cache_read_tokens {
        if cached > 0 {
            lines.push(format!("{cached} tokens read from cache"));
        }
    }

    lines.push(format!(
        "{} request(s) in {}",
        usage.request_count,
        format_duration(elapsed)
    ));

    lines.join("\n")
}

/// A duration in the units a person reads at a glance.
fn format_duration(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs_f64();

    if secs < 1.0 {
        format!("{}ms", elapsed.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m {}s", elapsed.as_secs() / 60, elapsed.as_secs() % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally_at_each_scale() {
        assert_eq!(
            format_duration(std::time::Duration::from_millis(250)),
            "250ms"
        );
        assert_eq!(
            format_duration(std::time::Duration::from_millis(1500)),
            "1.5s"
        );
        assert_eq!(
            format_duration(std::time::Duration::from_secs(90)),
            "1m 30s"
        );
    }

    #[test]
    fn a_turn_emits_a_divider_and_starts_a_spinner() {
        let bus = Arc::new(MessageBus::new());
        let _turn = Turn::begin(Arc::clone(&bus));

        let messages = bus.get_buffered_messages();

        assert!(messages.iter().any(|m| matches!(m, AnyMessage::Divider(_))));
        assert!(messages.iter().any(|m| matches!(
            m,
            AnyMessage::SpinnerControl(s) if s.action == SpinnerAction::Start
        )));
    }

    #[test]
    fn progress_updates_the_same_spinner() {
        // A new id per update would leave the previous spinner running.
        let bus = Arc::new(MessageBus::new());
        let turn = Turn::begin(Arc::clone(&bus));
        turn.progress("Reading files");

        let ids: Vec<String> = bus
            .get_buffered_messages()
            .iter()
            .filter_map(|m| match m {
                AnyMessage::SpinnerControl(s) => Some(s.spinner_id.clone()),
                _ => None,
            })
            .collect();

        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], ids[1]);
    }

    #[test]
    fn a_failed_turn_still_stops_the_spinner() {
        // Otherwise the interface claims to be working on something it abandoned.
        let bus = Arc::new(MessageBus::new());
        let turn = Turn::begin(Arc::clone(&bus));
        turn.fail("it broke");

        let messages = bus.get_buffered_messages();

        assert!(messages.iter().any(|m| matches!(
            m,
            AnyMessage::SpinnerControl(s) if s.action == SpinnerAction::Stop
        )));
        assert!(messages.iter().any(|m| matches!(
            m,
            AnyMessage::StatusPanel(p) if p.status_type == StatusType::Error
        )));
    }

    #[test]
    fn the_answer_is_emitted_before_its_cost_summary() {
        // The summary describes the answer, so printing it first reads as a
        // report about something the user has not seen yet.
        let bus = Arc::new(MessageBus::new());
        let turn = Turn::begin(Arc::clone(&bus));

        turn.finish(&AgentRunResult {
            output: "the answer".to_string(),
            messages: Vec::new(),
            responses: Vec::new(),
            usage: Default::default(),
            run_id: String::new(),
            finish_reason: serdes_ai_core::FinishReason::Stop,
            metadata: None,
        });

        let messages = bus.get_buffered_messages();
        let answer = messages
            .iter()
            .position(|m| matches!(m, AnyMessage::AgentResponse(_)))
            .expect("the answer was never emitted");
        let summary = messages
            .iter()
            .position(|m| matches!(m, AnyMessage::StatusPanel(_)))
            .expect("the summary was never emitted");

        assert!(answer < summary, "the cost summary preceded the answer");
    }

    #[test]
    fn an_unreported_usage_says_so_rather_than_showing_zero() {
        let result = AgentRunResult {
            output: String::new(),
            messages: Vec::new(),
            responses: Vec::new(),
            usage: Default::default(),
            run_id: String::new(),
            finish_reason: serdes_ai_core::FinishReason::Stop,
            metadata: None,
        };

        let summary = summarise(&result, std::time::Duration::from_secs(1));

        assert!(summary.contains("not reported"));
        assert!(!summary.contains("0 tokens"));
    }

    #[test]
    fn a_turn_with_no_thinking_shows_no_reasoning() {
        let result = AgentRunResult {
            output: String::new(),
            messages: Vec::new(),
            responses: Vec::new(),
            usage: Default::default(),
            run_id: String::new(),
            finish_reason: serdes_ai_core::FinishReason::Stop,
            metadata: None,
        };

        assert!(extract_reasoning(&result).is_none());
    }
}
