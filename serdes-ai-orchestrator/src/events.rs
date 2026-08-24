//! The event stream an orchestration publishes.
//!
//! Events are broadcast rather than returned so that a terminal UI, a test, and
//! a future web frontend can all observe the same run without this crate
//! knowing anything about them.

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

use crate::plan::{ApprovalDecision, Plan};
use crate::role::Role;

/// Identifies one agent *instance* within a run.
///
/// A role name alone is not enough: a gate runs several verifiers at once, and
/// an orchestrator may run two `code` agents concurrently. Consumers key their
/// display state off this, not off the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub Uuid);

impl AgentId {
    /// Allocate a fresh identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for AgentId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Short form: enough to disambiguate concurrent agents on screen.
        write!(f, "{:.8}", self.0.simple().to_string())
    }
}

/// Which text channel a delta belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaKind {
    /// Visible response text.
    Text,
    /// Reasoning text from a thinking-capable model.
    Thinking,
}

/// Where a tool call is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    /// The model has begun emitting a call to this tool.
    Started,
    /// The tool ran; `success` on the event says whether it worked.
    Executed,
}

/// Which mode a run is executing in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Planner, approval, orchestrator, and the quorum gate.
    Workflow,
    /// Orchestrator and subagents only; no gate.
    Fast,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::Workflow => "workflow",
            Mode::Fast => "fast",
        })
    }
}

/// A progress event from a running orchestration.
///
/// Every agent-scoped variant carries an [`AgentId`] so concurrent agents stay
/// distinguishable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OrchestratorEvent {
    /// A run began.
    ModeStarted {
        /// Which mode is running.
        mode: Mode,
        /// The task the user asked for.
        task: String,
    },
    /// The planner produced a plan for approval.
    PlanDrafted {
        /// The proposed plan.
        plan: Plan,
        /// Which attempt this is, counting from zero.
        attempt: u32,
    },
    /// The user decided about a proposed plan.
    PlanDecision {
        /// What they chose.
        decision: ApprovalDecision,
    },
    /// An agent instance was created and is about to run.
    AgentSpawned {
        /// The new agent.
        id: AgentId,
        /// The agent that spawned it, if any.
        parent: Option<AgentId>,
        /// The role it plays.
        role: Role,
        /// The task it was given.
        task: String,
    },
    /// Streaming text from an agent.
    AgentDelta {
        /// The agent producing text.
        id: AgentId,
        /// Whether this is response text or reasoning.
        kind: DeltaKind,
        /// The text fragment.
        text: String,
    },
    /// An agent used a tool.
    AgentTool {
        /// The agent calling the tool.
        id: AgentId,
        /// Tool name.
        tool: String,
        /// Lifecycle position.
        phase: ToolPhase,
        /// Whether it succeeded; `None` until the call completes.
        success: Option<bool>,
    },
    /// An agent finished normally.
    AgentFinished {
        /// The agent that finished.
        id: AgentId,
        /// Its final output.
        output: String,
    },
    /// An agent failed.
    AgentFailed {
        /// The agent that failed.
        id: AgentId,
        /// Why it failed.
        error: String,
    },
    /// A run ended.
    RunFinished {
        /// Whether the run succeeded.
        success: bool,
        /// Closing summary.
        summary: String,
    },
}
