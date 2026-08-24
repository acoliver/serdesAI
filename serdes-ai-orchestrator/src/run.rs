//! Running a single agent and publishing what it does.
//!
//! Every agent in an orchestration goes through [`run_agent`], which streams the
//! run and republishes each `AgentStreamEvent` as an [`OrchestratorEvent`] tagged
//! with the agent's [`AgentId`]. That tagging is what keeps concurrent agents
//! distinguishable downstream.

use futures::StreamExt;
use serdes_ai_agent::{Agent, AgentStreamEvent};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::events::{AgentId, DeltaKind, OrchestratorEvent, ToolPhase};
use crate::role::Role;

/// Why an agent run did not produce output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    /// The agent itself failed.
    #[error("{role} agent failed: {message}")]
    Agent {
        /// The role that failed.
        role: Role,
        /// The underlying message.
        message: String,
    },

    /// The run was cancelled before it finished.
    #[error("{role} agent was cancelled")]
    Cancelled {
        /// The role that was interrupted.
        role: Role,
    },
}

/// Publish `event`, ignoring the case where nobody is listening.
///
/// A run with no subscribers is normal — headless tests do not subscribe — and
/// must not be treated as a failure.
fn emit(events: &broadcast::Sender<OrchestratorEvent>, event: OrchestratorEvent) {
    let _ = events.send(event);
}

/// Run `agent` to completion, republishing its stream as orchestrator events.
///
/// Returns the agent's final text. Cancellation is checked cooperatively between
/// events, so a cancelled run stops at the next event boundary rather than
/// mid-request.
pub async fn run_agent(
    agent: &Agent<(), String>,
    id: AgentId,
    role: Role,
    parent: Option<AgentId>,
    task: String,
    events: &broadcast::Sender<OrchestratorEvent>,
    cancel: &CancellationToken,
) -> Result<String, RunError> {
    emit(
        events,
        OrchestratorEvent::AgentSpawned {
            id,
            parent,
            role,
            task: task.clone(),
        },
    );

    let mut stream = match agent.run_stream(task, ()).await {
        Ok(stream) => stream,
        Err(e) => {
            let message = e.to_string();
            emit(
                events,
                OrchestratorEvent::AgentFailed {
                    id,
                    error: message.clone(),
                },
            );
            return Err(RunError::Agent { role, message });
        }
    };

    let mut output = String::new();
    let mut failure: Option<String> = None;

    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                emit(events, OrchestratorEvent::AgentFailed {
                    id,
                    error: "cancelled".to_string(),
                });
                return Err(RunError::Cancelled { role });
            }
            next = stream.next() => next,
        };

        let Some(event) = next else { break };

        match event {
            Ok(AgentStreamEvent::TextDelta { text }) => {
                output.push_str(&text);
                emit(
                    events,
                    OrchestratorEvent::AgentDelta {
                        id,
                        kind: DeltaKind::Text,
                        text,
                    },
                );
            }
            Ok(AgentStreamEvent::ThinkingDelta { text }) => {
                emit(
                    events,
                    OrchestratorEvent::AgentDelta {
                        id,
                        kind: DeltaKind::Thinking,
                        text,
                    },
                );
            }
            Ok(AgentStreamEvent::ToolCallStart { tool_name, .. }) => {
                emit(
                    events,
                    OrchestratorEvent::AgentTool {
                        id,
                        tool: tool_name,
                        phase: ToolPhase::Started,
                        success: None,
                    },
                );
            }
            Ok(AgentStreamEvent::ToolExecuted {
                tool_name, success, ..
            }) => {
                emit(
                    events,
                    OrchestratorEvent::AgentTool {
                        id,
                        tool: tool_name,
                        phase: ToolPhase::Executed,
                        success: Some(success),
                    },
                );
            }
            Ok(AgentStreamEvent::Error { message }) => {
                failure = Some(message);
            }
            Ok(AgentStreamEvent::Cancelled { .. }) => {
                emit(
                    events,
                    OrchestratorEvent::AgentFailed {
                        id,
                        error: "cancelled".to_string(),
                    },
                );
                return Err(RunError::Cancelled { role });
            }
            Ok(_) => {}
            Err(e) => {
                failure = Some(e.to_string());
                break;
            }
        }
    }

    if let Some(message) = failure {
        emit(
            events,
            OrchestratorEvent::AgentFailed {
                id,
                error: message.clone(),
            },
        );
        return Err(RunError::Agent { role, message });
    }

    emit(
        events,
        OrchestratorEvent::AgentFinished {
            id,
            output: output.clone(),
        },
    );

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OrchestratorConfig;
    use crate::registry::tests::MockFactory;
    use crate::registry::AgentRegistry;
    use std::sync::Arc;

    fn registry(reply: &str) -> AgentRegistry {
        AgentRegistry::with_factory(
            OrchestratorConfig::new("."),
            Arc::new(MockFactory::new().replying(Role::Explore, reply)),
        )
    }

    /// Drain a receiver without blocking once the run is over.
    fn drain(rx: &mut broadcast::Receiver<OrchestratorEvent>) -> Vec<OrchestratorEvent> {
        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            seen.push(event);
        }
        seen
    }

    #[tokio::test]
    async fn returns_the_agents_output() {
        let reg = registry("the answer");
        let agent = reg.build(Role::Explore).unwrap();
        let (tx, _rx) = broadcast::channel(64);

        let out = run_agent(
            &agent,
            AgentId::new(),
            Role::Explore,
            None,
            "q".to_string(),
            &tx,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert_eq!(out, "the answer");
    }

    #[tokio::test]
    async fn publishes_spawn_and_finish_around_the_run() {
        let reg = registry("hello");
        let agent = reg.build(Role::Explore).unwrap();
        let (tx, mut rx) = broadcast::channel(64);
        let id = AgentId::new();

        run_agent(
            &agent,
            id,
            Role::Explore,
            None,
            "q".to_string(),
            &tx,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        let events = drain(&mut rx);

        assert!(matches!(
            events.first(),
            Some(OrchestratorEvent::AgentSpawned {
                role: Role::Explore,
                ..
            })
        ));
        assert!(matches!(
            events.last(),
            Some(OrchestratorEvent::AgentFinished { output, .. }) if output == "hello"
        ));
    }

    #[tokio::test]
    async fn tags_every_event_with_the_agents_id() {
        let reg = registry("hi");
        let agent = reg.build(Role::Explore).unwrap();
        let (tx, mut rx) = broadcast::channel(64);
        let id = AgentId::new();

        run_agent(
            &agent,
            id,
            Role::Explore,
            None,
            "q".to_string(),
            &tx,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        for event in drain(&mut rx) {
            let seen = match event {
                OrchestratorEvent::AgentSpawned { id, .. }
                | OrchestratorEvent::AgentDelta { id, .. }
                | OrchestratorEvent::AgentTool { id, .. }
                | OrchestratorEvent::AgentFinished { id, .. }
                | OrchestratorEvent::AgentFailed { id, .. } => Some(id),
                _ => None,
            };
            if let Some(seen) = seen {
                assert_eq!(seen, id, "event carried a different agent id");
            }
        }
    }

    #[tokio::test]
    async fn records_the_parent_on_spawn() {
        let reg = registry("x");
        let agent = reg.build(Role::Explore).unwrap();
        let (tx, mut rx) = broadcast::channel(64);
        let parent = AgentId::new();

        run_agent(
            &agent,
            AgentId::new(),
            Role::Explore,
            Some(parent),
            "q".to_string(),
            &tx,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        let spawned = drain(&mut rx)
            .into_iter()
            .find_map(|e| match e {
                OrchestratorEvent::AgentSpawned { parent, .. } => Some(parent),
                _ => None,
            })
            .expect("no spawn event");

        assert_eq!(spawned, Some(parent));
    }

    #[tokio::test]
    async fn a_run_with_no_subscribers_still_succeeds() {
        // Headless callers never subscribe; a full channel must not fail the run.
        let reg = registry("fine");
        let agent = reg.build(Role::Explore).unwrap();
        let (tx, rx) = broadcast::channel(1);
        drop(rx);

        let out = run_agent(
            &agent,
            AgentId::new(),
            Role::Explore,
            None,
            "q".to_string(),
            &tx,
            &CancellationToken::new(),
        )
        .await;

        assert_eq!(out.unwrap(), "fine");
    }

    #[tokio::test]
    async fn an_already_cancelled_token_stops_the_run() {
        let reg = registry("never seen");
        let agent = reg.build(Role::Explore).unwrap();
        let (tx, _rx) = broadcast::channel(64);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let err = run_agent(
            &agent,
            AgentId::new(),
            Role::Explore,
            None,
            "q".to_string(),
            &tx,
            &cancel,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            err,
            RunError::Cancelled {
                role: Role::Explore
            }
        ));
    }
}
