//! Agent roles.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// The role an agent plays in an orchestration.
///
/// A role determines three things: the system prompt the agent is built with,
/// the model it runs on, and — because `AgentBuilder` has no toolset support —
/// which tools are registered on it at build time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Drafts the plan that the user approves.
    Planner,
    /// Executes an approved plan by delegating to subagents.
    Orchestrator,
    /// Writes and edits code.
    Code,
    /// Reviews code without modifying it.
    Reviewer,
    /// Read-only investigation of the codebase.
    Explore,
    /// Independently judges finished work in the gate.
    Verifier,
}

impl Role {
    /// Every role, in declaration order.
    pub const ALL: [Role; 6] = [
        Role::Planner,
        Role::Orchestrator,
        Role::Code,
        Role::Reviewer,
        Role::Explore,
        Role::Verifier,
    ];

    /// The stable lowercase identifier used in config and tool arguments.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Planner => "planner",
            Role::Orchestrator => "orchestrator",
            Role::Code => "code",
            Role::Reviewer => "reviewer",
            Role::Explore => "explore",
            Role::Verifier => "verifier",
        }
    }

    /// Whether agents in this role may modify the working tree.
    ///
    /// Only [`Role::Code`] may write. Reviewers and verifiers deliberately get
    /// read-only access plus shell, so they can run tests but cannot "fix" what
    /// they are meant to be judging.
    pub fn can_write(self) -> bool {
        matches!(self, Role::Code)
    }

    /// Whether agents in this role may run shell commands.
    pub fn can_run_shell(self) -> bool {
        matches!(self, Role::Code | Role::Reviewer | Role::Verifier)
    }

    /// Roles an orchestrator is allowed to spawn.
    pub fn spawnable() -> [Role; 3] {
        [Role::Code, Role::Reviewer, Role::Explore]
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returned when a string does not name a known [`Role`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown role: {0}")]
pub struct UnknownRole(pub String);

impl FromStr for Role {
    type Err = UnknownRole;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "planner" => Ok(Role::Planner),
            "orchestrator" => Ok(Role::Orchestrator),
            "code" => Ok(Role::Code),
            "reviewer" => Ok(Role::Reviewer),
            "explore" => Ok(Role::Explore),
            "verifier" => Ok(Role::Verifier),
            _ => Err(UnknownRole(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_str() {
        for role in Role::ALL {
            assert_eq!(Role::from_str(role.as_str()), Ok(role));
        }
    }

    #[test]
    fn parsing_is_case_insensitive_and_trims() {
        assert_eq!(Role::from_str("  CODE "), Ok(Role::Code));
    }

    #[test]
    fn unknown_role_is_rejected() {
        assert!(Role::from_str("architect").is_err());
    }

    #[test]
    fn only_code_may_write() {
        for role in Role::ALL {
            assert_eq!(role.can_write(), role == Role::Code, "{role}");
        }
    }

    #[test]
    fn judging_roles_may_run_tests_but_not_write() {
        for role in [Role::Reviewer, Role::Verifier] {
            assert!(role.can_run_shell(), "{role} must be able to run tests");
            assert!(!role.can_write(), "{role} must not be able to write");
        }
    }
}
