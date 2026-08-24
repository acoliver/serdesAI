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

pub mod events;
pub mod role;
pub mod tools;

pub use events::{AgentId, DeltaKind, OrchestratorEvent, ToolPhase};
pub use role::Role;
pub use tools::{ToolContext, ToolFailure};

#[cfg(test)]
mod spike;
