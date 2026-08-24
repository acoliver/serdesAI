//! Bridges the orchestrator to the CLI's message bus and prompts.
//!
//! `serdes-ai-orchestrator` knows nothing about this crate: it publishes
//! [`OrchestratorEvent`] values and asks for decisions through traits. This
//! module is the only place the two meet.

pub mod run;

pub use run::run;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serdes_ai_orchestrator::events::{DeltaKind, OrchestratorEvent, ToolPhase};
use serdes_ai_orchestrator::plan::{ApprovalDecision, Plan, PlanApprover};
use tokio::sync::broadcast;

use crate::bus::{AnyMessage, MessageBus};
use crate::input::UserInputSystem;
use crate::messages::{
    BaseMessage, MessageCategory, MessageLevel, SubAgentInvocationMessage, SubAgentResponseMessage,
    SubAgentStatus, SubAgentStatusMessage, TextMessage,
};

/// Consume an orchestration's events and render them on the bus.
///
/// Returns when the event stream closes, which happens once the orchestrator is
/// dropped.
pub async fn forward_events(mut rx: broadcast::Receiver<OrchestratorEvent>, bus: Arc<MessageBus>) {
    loop {
        match rx.recv().await {
            Ok(event) => render(&event, &bus),
            // A slow consumer misses events rather than stalling the run; say so
            // instead of silently showing an incomplete picture.
            Err(broadcast::error::RecvError::Lagged(n)) => {
                bus.emit_warning(format!("{n} orchestration events were dropped"));
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

fn render(event: &OrchestratorEvent, bus: &MessageBus) {
    match event {
        OrchestratorEvent::ModeStarted { mode, task } => {
            bus.emit_info(format!("Starting {mode} mode: {task}"));
        }

        OrchestratorEvent::PlanDrafted { plan, attempt } => {
            let heading = if *attempt == 0 {
                "Proposed plan".to_string()
            } else {
                format!("Revised plan (attempt {})", attempt + 1)
            };
            bus.emit(AnyMessage::Text(TextMessage {
                base: BaseMessage::new(MessageCategory::Agent, None),
                level: MessageLevel::Info,
                text: format!("{heading}\n\n{plan}"),
            }));
        }

        OrchestratorEvent::PlanDecision { decision } => match decision {
            ApprovalDecision::Approve => bus.emit_success("Plan approved."),
            ApprovalDecision::Reject { feedback } => {
                bus.emit_info(format!("Plan rejected: {feedback}"));
            }
            ApprovalDecision::Cancel => bus.emit_warning("Plan cancelled."),
        },

        OrchestratorEvent::AgentSpawned {
            id,
            parent,
            role,
            task,
        } => {
            bus.emit(AnyMessage::SubAgentInvocation(SubAgentInvocationMessage {
                base: BaseMessage::new(MessageCategory::Agent, None),
                agent_id: Some(id.to_string()),
                parent_id: parent.map(|p| p.to_string()),
                agent_name: role.to_string(),
                prompt: task.clone(),
            }));
            status(
                bus,
                id.to_string(),
                role.to_string(),
                SubAgentStatus::Running,
            );
        }

        // Text deltas are high-volume and belong to whichever agent produced
        // them; showing them interleaved from several agents at once would be
        // unreadable, so they are left to the caller's transcript handling.
        OrchestratorEvent::AgentDelta { kind, .. } => {
            let _ = kind == &DeltaKind::Text;
        }

        OrchestratorEvent::AgentTool {
            tool,
            phase,
            success,
            ..
        } => {
            if matches!(phase, ToolPhase::Executed) {
                match success {
                    Some(false) => bus.emit_warning(format!("{tool} failed")),
                    _ => bus.emit_info(tool.to_string()),
                }
            }
        }

        OrchestratorEvent::AgentFinished { id, output } => {
            bus.emit(AnyMessage::SubAgentResponse(SubAgentResponseMessage {
                base: BaseMessage::new(MessageCategory::Agent, None),
                agent_id: Some(id.to_string()),
                agent_name: String::new(),
                response: output.clone(),
            }));
            status(
                bus,
                id.to_string(),
                String::new(),
                SubAgentStatus::Completed,
            );
        }

        OrchestratorEvent::AgentFailed { id, error } => {
            bus.emit_error(format!("Agent failed: {error}"));
            status(bus, id.to_string(), String::new(), SubAgentStatus::Failed);
        }

        OrchestratorEvent::GateRoundStart { round, verifiers } => {
            bus.emit_info(format!(
                "Verification round {round}: consulting {verifiers} verifiers"
            ));
        }

        OrchestratorEvent::GateVerdict { verdict, .. } => {
            let vote = if verdict.passes() { "accept" } else { "reject" };
            let detail = if verdict.summary.is_empty() {
                String::new()
            } else {
                format!(" — {}", verdict.summary)
            };
            bus.emit_info(format!("Verifier votes {vote}{detail}"));
        }

        OrchestratorEvent::GateResult {
            round,
            passed,
            total,
            quorum_met,
        } => {
            let line = format!("Round {round}: {passed} of {total} accepted");
            if *quorum_met {
                bus.emit_success(format!("{line} — quorum reached"));
            } else {
                bus.emit_warning(format!(
                    "{line} — quorum not reached, returning to the orchestrator"
                ));
            }
        }

        OrchestratorEvent::RunFinished { success, summary } => {
            if *success {
                bus.emit_success(format!("Run complete. {summary}"));
            } else {
                bus.emit_error(format!("Run did not complete. {summary}"));
            }
        }
    }
}

fn status(bus: &MessageBus, agent_id: String, agent_name: String, status: SubAgentStatus) {
    bus.emit(AnyMessage::SubAgentStatus(SubAgentStatusMessage {
        base: BaseMessage::new(MessageCategory::Agent, None),
        agent_id: Some(agent_id),
        agent_name,
        status,
        progress: None,
    }));
}

/// Asks the user to approve a plan through the CLI's existing prompts.
pub struct CliApprover {
    input: UserInputSystem,
}

impl CliApprover {
    /// Create an approver bound to `bus`.
    pub fn new(bus: Arc<MessageBus>) -> Self {
        Self {
            input: UserInputSystem::new(bus),
        }
    }

    async fn ask(&self) -> Result<ApprovalDecision> {
        let choice = self
            .input
            .select(
                "Proceed with this plan?",
                &[
                    "Approve — carry it out".to_string(),
                    "Reject — send it back with feedback".to_string(),
                    "Cancel — abandon the run".to_string(),
                ],
            )
            .await?;

        Ok(match choice {
            0 => ApprovalDecision::Approve,
            1 => {
                let feedback = self
                    .input
                    .input("What should the plan do differently?", None)
                    .await?;
                ApprovalDecision::Reject { feedback }
            }
            _ => ApprovalDecision::Cancel,
        })
    }
}

#[async_trait]
impl PlanApprover for CliApprover {
    async fn approve(&self, _plan: &Plan) -> ApprovalDecision {
        // The plan itself is already on screen: the orchestrator publishes
        // PlanDrafted before asking, and forward_events renders it.
        match self.ask().await {
            Ok(decision) => decision,
            // A broken prompt must not be read as consent.
            Err(_) => ApprovalDecision::Cancel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serdes_ai_orchestrator::events::{AgentId, Mode};
    use serdes_ai_orchestrator::gate::Verdict;
    use serdes_ai_orchestrator::role::Role;

    fn bus() -> Arc<MessageBus> {
        Arc::new(MessageBus::new())
    }

    fn rendered(event: OrchestratorEvent) -> Vec<AnyMessage> {
        let bus = bus();
        render(&event, &bus);
        bus.get_buffered_messages()
    }

    #[test]
    fn a_spawn_becomes_an_invocation_carrying_both_ids() {
        let id = AgentId::new();
        let parent = AgentId::new();

        let messages = rendered(OrchestratorEvent::AgentSpawned {
            id,
            parent: Some(parent),
            role: Role::Code,
            task: "write it".to_string(),
        });

        let invocation = messages
            .iter()
            .find_map(|m| match m {
                AnyMessage::SubAgentInvocation(msg) => Some(msg),
                _ => None,
            })
            .expect("no invocation message");

        assert_eq!(invocation.agent_id, Some(id.to_string()));
        assert_eq!(invocation.parent_id, Some(parent.to_string()));
        assert_eq!(invocation.agent_name, "code");
        assert_eq!(invocation.prompt, "write it");
    }

    #[test]
    fn concurrent_agents_of_one_role_stay_distinguishable() {
        // The reason the id fields exist: three verifiers share a role name.
        let first = AgentId::new();
        let second = AgentId::new();

        let a = rendered(OrchestratorEvent::AgentSpawned {
            id: first,
            parent: None,
            role: Role::Verifier,
            task: "verify".to_string(),
        });
        let b = rendered(OrchestratorEvent::AgentSpawned {
            id: second,
            parent: None,
            role: Role::Verifier,
            task: "verify".to_string(),
        });

        let id_of = |msgs: &[AnyMessage]| {
            msgs.iter()
                .find_map(|m| match m {
                    AnyMessage::SubAgentInvocation(msg) => msg.agent_id.clone(),
                    _ => None,
                })
                .unwrap()
        };

        assert_ne!(id_of(&a), id_of(&b));
    }

    #[test]
    fn a_finished_agent_reports_completion() {
        let messages = rendered(OrchestratorEvent::AgentFinished {
            id: AgentId::new(),
            output: "all done".to_string(),
        });

        assert!(messages.iter().any(|m| matches!(
            m,
            AnyMessage::SubAgentStatus(s) if s.status == SubAgentStatus::Completed
        )));
        assert!(messages.iter().any(|m| matches!(
            m,
            AnyMessage::SubAgentResponse(r) if r.response == "all done"
        )));
    }

    #[test]
    fn a_failed_agent_reports_failure() {
        let messages = rendered(OrchestratorEvent::AgentFailed {
            id: AgentId::new(),
            error: "boom".to_string(),
        });

        assert!(messages.iter().any(|m| matches!(
            m,
            AnyMessage::SubAgentStatus(s) if s.status == SubAgentStatus::Failed
        )));
    }

    #[test]
    fn the_plan_is_rendered_for_the_user_before_approval() {
        let plan = Plan {
            summary: "do the thing".to_string(),
            steps: vec![serdes_ai_orchestrator::plan::PlanStep {
                description: "step one".to_string(),
                files: vec![],
            }],
            verification: vec![],
            concerns: vec!["this is ambiguous".to_string()],
        };

        let messages = rendered(OrchestratorEvent::PlanDrafted { plan, attempt: 0 });

        let text = messages
            .iter()
            .find_map(|m| match m {
                AnyMessage::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .expect("the plan was not rendered");

        assert!(text.contains("Proposed plan"));
        assert!(text.contains("do the thing"));
        // A planner's concern must reach the person deciding.
        assert!(text.contains("this is ambiguous"));
    }

    #[test]
    fn a_revised_plan_says_which_attempt_it_is() {
        let plan = Plan {
            summary: "second try".to_string(),
            steps: vec![serdes_ai_orchestrator::plan::PlanStep {
                description: "s".to_string(),
                files: vec![],
            }],
            verification: vec![],
            concerns: vec![],
        };

        let messages = rendered(OrchestratorEvent::PlanDrafted { plan, attempt: 1 });
        let text = messages
            .iter()
            .find_map(|m| match m {
                AnyMessage::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .unwrap();

        assert!(text.contains("attempt 2"), "{text}");
    }

    #[test]
    fn a_failing_gate_round_is_reported_as_a_warning() {
        let messages = rendered(OrchestratorEvent::GateResult {
            round: 1,
            passed: 1,
            total: 3,
            quorum_met: false,
        });

        let text = messages
            .iter()
            .find_map(|m| match m {
                AnyMessage::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .unwrap();

        assert!(text.contains("1 of 3"));
        assert!(text.contains("quorum not reached"));
    }

    #[test]
    fn a_verdict_shows_which_way_it_voted() {
        let messages = rendered(OrchestratorEvent::GateVerdict {
            id: AgentId::new(),
            verdict: Verdict {
                complete: false,
                correct: true,
                clean: true,
                findings: vec![],
                summary: "step 2 is missing".to_string(),
            },
        });

        let text = messages
            .iter()
            .find_map(|m| match m {
                AnyMessage::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .unwrap();

        assert!(text.contains("reject"));
        assert!(text.contains("step 2 is missing"));
    }

    #[test]
    fn mode_start_names_the_mode() {
        let messages = rendered(OrchestratorEvent::ModeStarted {
            mode: Mode::Workflow,
            task: "build it".to_string(),
        });

        let text = messages
            .iter()
            .find_map(|m| match m {
                AnyMessage::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .unwrap();

        assert!(text.contains("workflow"));
        assert!(text.contains("build it"));
    }
}
