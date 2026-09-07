//! Multi-agent orchestration for `serdes-ai`.
//!
//! Two modes are provided:
//!
//! - **Workflow Mode** — a planner drafts a [`Plan`], the caller approves it via
//!   [`PlanApprover`], an orchestrator executes it through subagents, and a
//!   quorum [`gate`] of independent verifiers judges the result against the plan
//!   and the working tree. Failing quorum returns dissent to the orchestrator
//!   and the cycle repeats up to a configured cap.
//! - **Fast Mode** — the same agents and tools without the gate.
//!
//! The crate is deliberately UI-agnostic: progress is published as
//! [`OrchestratorEvent`] values on a broadcast channel, and human interaction
//! goes through the [`PlanApprover`] and [`ToolApprover`] traits. Nothing here
//! depends on the CLI.

pub mod config;
pub mod events;
pub mod evidence;
pub mod gate;
pub mod orchestrator;
pub mod plan;
pub mod prompts;
pub mod registry;
pub mod role;
pub mod run;
pub mod tools;

pub use config::{GateConfig, OrchestratorConfig, RoleConfig};
pub use events::{AgentId, DeltaKind, Mode, OrchestratorEvent, ToolPhase};
pub use evidence::Evidence;
pub use gate::{Finding, GateOutcome, Severity, Verdict};
pub use orchestrator::{OrchestrationError, Orchestrator, WorkflowOutcome};
pub use plan::{ApprovalDecision, AutoApprove, Plan, PlanApprover, PlanStep};
pub use registry::{AgentRegistry, ModelFactory};
pub use role::Role;
pub use run::{RunError, run_agent};
pub use tools::{ToolContext, ToolFailure};

#[cfg(test)]
mod spike;
