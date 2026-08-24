//! The orchestration entry point.

use std::sync::Arc;

use serde::Deserialize;
use serdes_ai_agent::RunContext;
use serdes_ai_models::ModelError;
use serdes_ai_tools::{ToolError, ToolReturn};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::config::{ConfigError, OrchestratorConfig};
use crate::events::{AgentId, Mode, OrchestratorEvent};
use crate::evidence;
use crate::gate::{self, GateOutcome, Verdict};
use crate::plan::{ApprovalDecision, Plan, PlanApprover};
use crate::registry::{AgentRegistry, ModelFactory};
use crate::role::Role;
use crate::run::{run_agent, RunError};
use crate::tools;

/// How many events may queue before a slow subscriber starts missing them.
const EVENT_CAPACITY: usize = 1024;

/// An orchestration that could not be completed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrchestrationError {
    /// The configuration was unusable.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// An agent could not be built.
    #[error("could not build {role} agent: {message}")]
    Build {
        /// The role that could not be built.
        role: Role,
        /// Why.
        message: String,
    },

    /// An agent run failed.
    #[error(transparent)]
    Run(#[from] RunError),

    /// The user abandoned the run at the approval step.
    #[error("the run was cancelled at plan approval")]
    PlanCancelled,

    /// The planner never produced a plan the user would accept.
    #[error("no plan was approved after {attempts} attempts")]
    PlanRejected {
        /// How many plans were put to the user.
        attempts: u32,
    },

    /// The planner returned a plan with no steps.
    #[error("the planner produced a plan with no steps")]
    EmptyPlan,
}

/// The result of a Workflow Mode run.
#[derive(Debug, Clone)]
pub struct WorkflowOutcome {
    /// Whether the gate accepted the work.
    pub accepted: bool,
    /// How many orchestrate-then-verify rounds were run.
    pub rounds: u32,
    /// The orchestrator's closing summary from the last round.
    pub summary: String,
    /// The final gate tally, if the gate ran.
    pub gate: Option<GateOutcome>,
}

#[derive(Debug, Deserialize)]
struct SpawnArgs {
    role: String,
    task: String,
    #[serde(default)]
    context: Option<String>,
}

/// Runs multi-agent orchestrations.
pub struct Orchestrator {
    registry: Arc<AgentRegistry>,
    events: broadcast::Sender<OrchestratorEvent>,
}

impl Orchestrator {
    /// Create an orchestrator resolving models from their configured specs.
    pub fn new(config: OrchestratorConfig) -> Result<Self, OrchestrationError> {
        config.validate()?;
        Ok(Self::from_registry(AgentRegistry::new(config)))
    }

    /// Create an orchestrator backed by a custom [`ModelFactory`].
    pub fn with_factory(
        config: OrchestratorConfig,
        factory: Arc<dyn ModelFactory>,
    ) -> Result<Self, OrchestrationError> {
        config.validate()?;
        Ok(Self::from_registry(AgentRegistry::with_factory(
            config, factory,
        )))
    }

    fn from_registry(registry: AgentRegistry) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            registry: Arc::new(registry),
            events,
        }
    }

    /// Observe this orchestrator's progress.
    ///
    /// Subscribe before starting a run; events published with no subscribers are
    /// dropped rather than buffered.
    pub fn subscribe(&self) -> broadcast::Receiver<OrchestratorEvent> {
        self.events.subscribe()
    }

    /// The registry backing this orchestrator.
    pub fn registry(&self) -> &Arc<AgentRegistry> {
        &self.registry
    }

    /// Run `task` in Fast Mode: an orchestrator delegating to subagents, with no
    /// plan approval and no gate.
    pub async fn run_fast(
        &self,
        task: &str,
        cancel: CancellationToken,
    ) -> Result<String, OrchestrationError> {
        let _ = self.events.send(OrchestratorEvent::ModeStarted {
            mode: Mode::Fast,
            task: task.to_string(),
        });

        let id = AgentId::new();
        let result = self.run_orchestrator(id, task.to_string(), &cancel).await;

        let _ = self.events.send(OrchestratorEvent::RunFinished {
            success: result.is_ok(),
            summary: match &result {
                Ok(output) => output.clone(),
                Err(e) => e.to_string(),
            },
        });

        result
    }

    /// Run `task` in Workflow Mode: plan, approval, execution, then the gate.
    ///
    /// The orchestrator is sent back around with the verifiers' dissent until
    /// quorum is reached or `gate.max_rounds` is exhausted. Exhausting the rounds
    /// is reported with the accumulated findings rather than as a bare failure —
    /// the user needs to see what the verifiers objected to.
    pub async fn run_workflow(
        &self,
        task: &str,
        approver: &dyn PlanApprover,
        cancel: CancellationToken,
    ) -> Result<WorkflowOutcome, OrchestrationError> {
        let _ = self.events.send(OrchestratorEvent::ModeStarted {
            mode: Mode::Workflow,
            task: task.to_string(),
        });

        let result = self.workflow_inner(task, approver, &cancel).await;

        let _ = self.events.send(OrchestratorEvent::RunFinished {
            success: matches!(&result, Ok(o) if o.accepted),
            summary: match &result {
                Ok(outcome) => outcome.summary.clone(),
                Err(e) => e.to_string(),
            },
        });

        result
    }

    async fn workflow_inner(
        &self,
        task: &str,
        approver: &dyn PlanApprover,
        cancel: &CancellationToken,
    ) -> Result<WorkflowOutcome, OrchestrationError> {
        let plan = self.plan(task, approver, cancel).await?;
        let gate_config = self.registry.config().gate.clone();

        let mut feedback: Option<String> = None;
        let mut last_outcome: Option<GateOutcome> = None;
        let mut summary = String::new();

        for round in 1..=gate_config.max_rounds {
            if cancel.is_cancelled() {
                return Err(OrchestrationError::Run(RunError::Cancelled {
                    role: Role::Orchestrator,
                }));
            }

            let prompt = match &feedback {
                None => {
                    format!("Carry out this approved plan.\n\n{plan}\n\nOriginal request: {task}")
                }
                Some(notes) => format!(
                    "Your previous attempt did not pass verification.\n\n{notes}\n\n\
                     The approved plan is unchanged:\n\n{plan}\n\nOriginal request: {task}"
                ),
            };

            let id = AgentId::new();
            summary = self.run_orchestrator(id, prompt, cancel).await?;

            let outcome = self.run_gate(&plan, round, cancel).await?;

            let _ = self.events.send(OrchestratorEvent::GateResult {
                round,
                passed: outcome.passed,
                total: outcome.total(),
                quorum_met: outcome.quorum_met,
            });

            if outcome.quorum_met {
                return Ok(WorkflowOutcome {
                    accepted: true,
                    rounds: round,
                    summary,
                    gate: Some(outcome),
                });
            }

            feedback = Some(outcome.feedback());
            last_outcome = Some(outcome);
        }

        Ok(WorkflowOutcome {
            accepted: false,
            rounds: gate_config.max_rounds,
            summary,
            gate: last_outcome,
        })
    }

    /// Run one gate round: gather evidence once, then vote on it.
    async fn run_gate(
        &self,
        plan: &Plan,
        round: u32,
        cancel: &CancellationToken,
    ) -> Result<GateOutcome, OrchestrationError> {
        let config = self.registry.config();
        let verifier_count = config.gate.verifiers;

        let _ = self.events.send(OrchestratorEvent::GateRoundStart {
            round,
            verifiers: verifier_count,
        });

        // Gathered once and shared: every verifier must judge the same tree, or
        // they are not voting on the same thing.
        let evidence = evidence::collect(&config.root, config.gate.test_command.as_deref()).await;

        let votes = (0..verifier_count).map(|index| {
            let prompt = gate::verifier_prompt(plan, &evidence, index);
            async move { self.run_verifier(prompt, cancel).await }
        });

        // Verifiers are independent, so they run concurrently.
        let results = futures::future::join_all(votes).await;

        let verdicts: Vec<Verdict> = results
            .into_iter()
            .filter_map(|r| match r {
                Ok(verdict) => Some(verdict),
                Err(e) => {
                    tracing::warn!("a verifier failed to report: {e}");
                    None
                }
            })
            .collect();

        Ok(GateOutcome::tally(verdicts, verifier_count))
    }

    /// Run one verifier and collect its verdict.
    async fn run_verifier(
        &self,
        prompt: String,
        cancel: &CancellationToken,
    ) -> Result<Verdict, OrchestrationError> {
        if cancel.is_cancelled() {
            return Err(OrchestrationError::Run(RunError::Cancelled {
                role: Role::Verifier,
            }));
        }

        let builder =
            self.registry
                .builder_for(Role::Verifier)
                .map_err(|e| OrchestrationError::Build {
                    role: Role::Verifier,
                    message: e.to_string(),
                })?;

        // A verifier reads and runs tests but cannot write: it must not be able
        // to repair the work it is judging.
        let agent = tools::register_for_role(
            builder,
            Role::Verifier,
            self.registry.tool_context().clone(),
        )
        .output_tool::<Verdict>(gate::VERDICT_TOOL, Verdict::schema())
        .build();

        let id = AgentId::new();
        let _ = self.events.send(OrchestratorEvent::AgentSpawned {
            id,
            parent: None,
            role: Role::Verifier,
            task: "verify the work against the plan".to_string(),
        });

        let result = agent.run(prompt, ()).await.map_err(|e| {
            OrchestrationError::Run(RunError::Agent {
                role: Role::Verifier,
                message: e.to_string(),
            })
        })?;

        let _ = self.events.send(OrchestratorEvent::GateVerdict {
            id,
            verdict: result.output.clone(),
        });

        Ok(result.output)
    }

    /// Draft a plan for `task` and put it to `approver`.
    ///
    /// A rejection returns to the planner with the feedback attached, up to
    /// `max_plan_revisions` times. The cap exists so a user who keeps rejecting
    /// gets an answer rather than an unbounded loop.
    pub async fn plan(
        &self,
        task: &str,
        approver: &dyn PlanApprover,
        cancel: &CancellationToken,
    ) -> Result<Plan, OrchestrationError> {
        let max_revisions = self.registry.config().max_plan_revisions;
        let mut feedback: Vec<String> = Vec::new();

        for attempt in 0..=max_revisions {
            if cancel.is_cancelled() {
                return Err(OrchestrationError::Run(RunError::Cancelled {
                    role: Role::Planner,
                }));
            }

            let prompt = if feedback.is_empty() {
                task.to_string()
            } else {
                format!(
                    "{task}\n\nA previous plan was rejected. Address this feedback:\n{}",
                    feedback
                        .iter()
                        .map(|f| format!("- {f}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            };

            let plan = self.draft_plan(prompt, cancel).await?;

            let _ = self.events.send(OrchestratorEvent::PlanDrafted {
                plan: plan.clone(),
                attempt,
            });

            let decision = approver.approve(&plan).await;

            let _ = self.events.send(OrchestratorEvent::PlanDecision {
                decision: decision.clone(),
            });

            match decision {
                ApprovalDecision::Approve => return Ok(plan),
                ApprovalDecision::Cancel => {
                    return Err(OrchestrationError::PlanCancelled);
                }
                ApprovalDecision::Reject { feedback: note } => feedback.push(note),
            }
        }

        Err(OrchestrationError::PlanRejected {
            attempts: max_revisions + 1,
        })
    }

    /// Run the planner once and deserialize its plan.
    async fn draft_plan(
        &self,
        prompt: String,
        cancel: &CancellationToken,
    ) -> Result<Plan, OrchestrationError> {
        let builder =
            self.registry
                .builder_for(Role::Planner)
                .map_err(|e| OrchestrationError::Build {
                    role: Role::Planner,
                    message: e.to_string(),
                })?;

        // The planner investigates before planning, so it gets read-only tools,
        // and returns a Plan rather than prose so the gate has something
        // structured to check against later.
        let agent =
            tools::register_for_role(builder, Role::Planner, self.registry.tool_context().clone())
                .output_tool::<Plan>(crate::plan::PLAN_TOOL, Plan::schema())
                .build();

        let id = AgentId::new();
        let _ = self.events.send(OrchestratorEvent::AgentSpawned {
            id,
            parent: None,
            role: Role::Planner,
            task: prompt.clone(),
        });

        if cancel.is_cancelled() {
            return Err(OrchestrationError::Run(RunError::Cancelled {
                role: Role::Planner,
            }));
        }

        let result = agent.run(prompt, ()).await.map_err(|e| {
            OrchestrationError::Run(RunError::Agent {
                role: Role::Planner,
                message: e.to_string(),
            })
        })?;

        let _ = self.events.send(OrchestratorEvent::AgentFinished {
            id,
            output: result.output.summary.clone(),
        });

        if result.output.is_empty() {
            return Err(OrchestrationError::EmptyPlan);
        }

        Ok(result.output)
    }

    /// Build the orchestrator agent and run it.
    async fn run_orchestrator(
        &self,
        id: AgentId,
        task: String,
        cancel: &CancellationToken,
    ) -> Result<String, OrchestrationError> {
        let agent = self.build_orchestrator_agent(id, cancel.clone())?;

        Ok(run_agent(
            &agent,
            id,
            Role::Orchestrator,
            None,
            task,
            &self.events,
            cancel,
        )
        .await?)
    }

    /// Build the orchestrator agent, giving it the `spawn_agent` tool.
    ///
    /// The orchestrator itself gets only read-only tools ([`Role::can_write`] is
    /// false for it): it delegates changes rather than making them, so that every
    /// edit is attributable to a subagent the gate can inspect.
    fn build_orchestrator_agent(
        &self,
        parent: AgentId,
        cancel: CancellationToken,
    ) -> Result<serdes_ai_agent::Agent<(), String>, OrchestrationError> {
        let role_config = self.registry.config().role(Role::Orchestrator);

        let builder = self
            .registry
            .builder_for(Role::Orchestrator)
            .map_err(|e: ModelError| OrchestrationError::Build {
                role: Role::Orchestrator,
                message: e.to_string(),
            })?;

        let builder = tools::register_for_role(
            builder,
            Role::Orchestrator,
            self.registry.tool_context().clone(),
        );

        let builder = if let Some(max_tokens) = role_config.max_tokens {
            builder.max_tokens(max_tokens)
        } else {
            builder
        };

        let registry = Arc::clone(&self.registry);
        let events = self.events.clone();

        Ok(builder
            .parallel_tool_calls(true)
            .tool_fn_async(
                "spawn_agent",
                "Delegate a self-contained task to a subagent. \
                 role is one of: code, reviewer, explore. \
                 The subagent sees only what you put in task and context.",
                move |_c: &RunContext<()>, args: SpawnArgs| {
                    // Cloned per call: the closure is Fn and the future must be
                    // 'static, so nothing may be borrowed from here or from _c.
                    let registry = Arc::clone(&registry);
                    let events = events.clone();
                    let cancel = cancel.clone();

                    async move {
                        let role: Role = args.role.parse().map_err(|_| {
                            ToolError::execution_failed(format!(
                                "unknown role '{}'; expected one of: code, reviewer, explore",
                                args.role
                            ))
                        })?;

                        if !Role::spawnable().contains(&role) {
                            return Err(ToolError::execution_failed(format!(
                                "{role} cannot be spawned; expected one of: code, reviewer, explore"
                            )));
                        }

                        let agent = registry.build(role).map_err(|e| {
                            ToolError::execution_failed(format!(
                                "could not build {role} agent: {e}"
                            ))
                        })?;

                        let prompt = match args.context {
                            Some(context) if !context.trim().is_empty() => {
                                format!("{}\n\nContext:\n{}", args.task, context)
                            }
                            _ => args.task,
                        };

                        let output = run_agent(
                            &agent,
                            AgentId::new(),
                            role,
                            Some(parent),
                            prompt,
                            &events,
                            &cancel,
                        )
                        .await
                        .map_err(|e| ToolError::execution_failed(e.to_string()))?;

                        Ok(ToolReturn::text(output))
                    }
                },
            )
            .build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GateConfig;
    use crate::registry::tests::MockFactory;

    fn orchestrator(factory: MockFactory) -> Orchestrator {
        Orchestrator::with_factory(OrchestratorConfig::new("."), Arc::new(factory)).unwrap()
    }

    fn drain(rx: &mut broadcast::Receiver<OrchestratorEvent>) -> Vec<OrchestratorEvent> {
        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            seen.push(event);
        }
        seen
    }

    #[tokio::test]
    async fn fast_mode_returns_the_orchestrators_output() {
        let orch = orchestrator(MockFactory::new().replying(Role::Orchestrator, "all done"));

        let out = orch
            .run_fast("do a thing", CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(out, "all done");
    }

    #[tokio::test]
    async fn fast_mode_brackets_the_run_with_mode_and_finish_events() {
        let orch = orchestrator(MockFactory::new().replying(Role::Orchestrator, "done"));
        let mut rx = orch.subscribe();

        orch.run_fast("task", CancellationToken::new())
            .await
            .unwrap();

        let events = drain(&mut rx);

        assert!(matches!(
            events.first(),
            Some(OrchestratorEvent::ModeStarted {
                mode: Mode::Fast,
                ..
            })
        ));
        assert!(matches!(
            events.last(),
            Some(OrchestratorEvent::RunFinished { success: true, .. })
        ));
    }

    #[tokio::test]
    async fn the_orchestrator_agent_has_spawn_agent_but_no_write_tools() {
        let orch = orchestrator(MockFactory::new());
        let agent = orch
            .build_orchestrator_agent(AgentId::new(), CancellationToken::new())
            .unwrap();

        let names: Vec<&str> = agent.tools().iter().map(|d| d.name.as_str()).collect();

        assert!(names.contains(&"spawn_agent"));
        assert!(
            !names.contains(&"write_file"),
            "the orchestrator must delegate edits, not make them: {names:?}"
        );
        assert!(!names.contains(&"bash"), "{names:?}");
    }

    #[tokio::test]
    async fn a_failing_run_reports_failure_in_the_finish_event() {
        struct Failing;
        impl ModelFactory for Failing {
            fn model_for(
                &self,
                _r: Role,
                _s: &str,
            ) -> Result<Arc<dyn serdes_ai_models::Model>, ModelError> {
                Err(ModelError::configuration("no model"))
            }
        }

        let orch =
            Orchestrator::with_factory(OrchestratorConfig::new("."), Arc::new(Failing)).unwrap();
        let mut rx = orch.subscribe();

        let result = orch.run_fast("task", CancellationToken::new()).await;

        assert!(result.is_err());
        assert!(matches!(
            drain(&mut rx).last(),
            Some(OrchestratorEvent::RunFinished { success: false, .. })
        ));
    }

    #[tokio::test]
    async fn cancellation_stops_the_run() {
        let orch = orchestrator(MockFactory::new());
        let cancel = CancellationToken::new();
        cancel.cancel();

        let err = orch.run_fast("task", cancel).await.unwrap_err();

        assert!(matches!(
            err,
            OrchestrationError::Run(RunError::Cancelled { .. })
        ));
    }

    #[test]
    fn an_invalid_gate_config_is_rejected_at_construction() {
        let config = OrchestratorConfig::new(".").with_gate(GateConfig {
            verifiers: 1,
            ..Default::default()
        });

        assert!(matches!(
            Orchestrator::new(config),
            Err(OrchestrationError::Config(
                ConfigError::TooFewVerifiers { .. }
            ))
        ));
    }
}
