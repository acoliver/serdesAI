//! Configuration for an orchestration run.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::role::Role;

/// Default model used by any role without a specific override.
pub const DEFAULT_MODEL: &str = "anthropic:claude-sonnet-4-5";

/// Model defaults per role.
///
/// Explore is the highest-volume role and its work is mechanical, so it gets a
/// cheap model. The planner and the gate's verifiers carry the most consequence
/// — a bad plan or a rubber-stamped verdict costs far more than the token
/// difference — so they get the strongest.
fn default_model_for(role: Role) -> &'static str {
    match role {
        Role::Explore => "anthropic:claude-haiku-4-5",
        Role::Planner | Role::Verifier => "anthropic:claude-opus-4-5",
        Role::Orchestrator | Role::Code | Role::Reviewer => DEFAULT_MODEL,
    }
}

/// Per-role settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleConfig {
    /// Model specification, e.g. `"anthropic:claude-sonnet-4-5"`.
    pub model: String,
    /// System prompt. Defaults to the built-in prompt for the role.
    pub system_prompt: String,
    /// Cap on tokens for a single agent of this role, if any.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Cap on model requests for a single agent of this role.
    ///
    /// The agent loop ends on a completion signal, so an agent whose model keeps
    /// returning tool calls never terminates on its own. This bounds that.
    #[serde(default = "default_max_requests")]
    pub max_requests: u32,
}

/// Default cap on model requests within a single agent run.
fn default_max_requests() -> u32 {
    50
}

impl RoleConfig {
    /// Built-in defaults for `role`.
    pub fn defaults_for(role: Role) -> Self {
        Self {
            model: default_model_for(role).to_string(),
            system_prompt: crate::prompts::for_role(role).to_string(),
            max_tokens: None,
            max_requests: default_max_requests(),
        }
    }
}

/// Gate settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateConfig {
    /// How many verifiers vote. Must be at least 3.
    pub verifiers: usize,
    /// How many times the orchestrator may be sent back before giving up.
    pub max_rounds: u32,
    /// Command used to gather build/test evidence, if any.
    #[serde(default)]
    pub test_command: Option<String>,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            verifiers: 3,
            max_rounds: 3,
            test_command: None,
        }
    }
}

impl GateConfig {
    /// Minimum permitted verifier count.
    pub const MIN_VERIFIERS: usize = 3;

    /// Whether `passed` out of `total` clears the bar.
    ///
    /// Strictly more than half, computed without floating point so an even
    /// verifier count cannot be rounded into a pass: 2 of 4 fails.
    pub fn quorum_met(passed: usize, total: usize) -> bool {
        total > 0 && passed * 2 > total
    }

    /// Reject a configuration that cannot produce a meaningful vote.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.verifiers < Self::MIN_VERIFIERS {
            return Err(ConfigError::TooFewVerifiers {
                requested: self.verifiers,
                minimum: Self::MIN_VERIFIERS,
            });
        }
        if self.max_rounds == 0 {
            return Err(ConfigError::ZeroRounds);
        }
        Ok(())
    }
}

/// An unusable configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// Fewer verifiers than the minimum.
    #[error("gate needs at least {minimum} verifiers, got {requested}")]
    TooFewVerifiers {
        /// What was asked for.
        requested: usize,
        /// The floor.
        minimum: usize,
    },

    /// A gate that may never run is a gate that never gates.
    #[error("gate max_rounds must be at least 1")]
    ZeroRounds,
}

/// Everything an orchestration run needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    /// Directory agents read, write and run commands in.
    pub root: PathBuf,
    /// Per-role overrides. Missing roles fall back to [`RoleConfig::defaults_for`].
    #[serde(default)]
    pub roles: HashMap<Role, RoleConfig>,
    /// Gate settings.
    #[serde(default)]
    pub gate: GateConfig,
    /// How many times a rejected plan may be sent back to the planner before
    /// the run gives up.
    #[serde(default = "default_plan_revisions")]
    pub max_plan_revisions: u32,
}

/// Default cap on planner revisions.
fn default_plan_revisions() -> u32 {
    3
}

impl OrchestratorConfig {
    /// A configuration rooted at `root` using every built-in default.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            roles: HashMap::new(),
            gate: GateConfig::default(),
            max_plan_revisions: default_plan_revisions(),
        }
    }

    /// Settings for `role`, falling back to the built-in defaults.
    pub fn role(&self, role: Role) -> RoleConfig {
        self.roles
            .get(&role)
            .cloned()
            .unwrap_or_else(|| RoleConfig::defaults_for(role))
    }

    /// Override the model for one role.
    pub fn with_model(mut self, role: Role, model: impl Into<String>) -> Self {
        let mut cfg = self.role(role);
        cfg.model = model.into();
        self.roles.insert(role, cfg);
        self
    }

    /// Override the gate settings.
    pub fn with_gate(mut self, gate: GateConfig) -> Self {
        self.gate = gate;
        self
    }

    /// Check the configuration is usable.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.gate.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quorum_needs_strictly_more_than_half() {
        // Odd counts: a clear majority.
        assert!(GateConfig::quorum_met(2, 3));
        assert!(!GateConfig::quorum_met(1, 3));
        assert!(GateConfig::quorum_met(3, 5));
        assert!(!GateConfig::quorum_met(2, 5));
    }

    #[test]
    fn an_even_split_does_not_pass() {
        // The case a naive `passed >= total / 2` would wrongly let through.
        assert!(!GateConfig::quorum_met(2, 4));
        assert!(GateConfig::quorum_met(3, 4));
        assert!(!GateConfig::quorum_met(3, 6));
        assert!(GateConfig::quorum_met(4, 6));
    }

    #[test]
    fn unanimity_and_shutout_behave() {
        assert!(GateConfig::quorum_met(5, 5));
        assert!(!GateConfig::quorum_met(0, 5));
    }

    #[test]
    fn no_verifiers_never_passes() {
        // Guards against an empty vote being read as consensus.
        assert!(!GateConfig::quorum_met(0, 0));
    }

    #[test]
    fn rejects_too_few_verifiers() {
        let gate = GateConfig {
            verifiers: 2,
            ..Default::default()
        };

        assert!(matches!(
            gate.validate(),
            Err(ConfigError::TooFewVerifiers { .. })
        ));
    }

    #[test]
    fn accepts_more_than_three_verifiers() {
        let gate = GateConfig {
            verifiers: 7,
            ..Default::default()
        };

        assert!(gate.validate().is_ok());
    }

    #[test]
    fn rejects_zero_rounds() {
        let gate = GateConfig {
            max_rounds: 0,
            ..Default::default()
        };

        assert!(matches!(gate.validate(), Err(ConfigError::ZeroRounds)));
    }

    #[test]
    fn roles_fall_back_to_defaults() {
        let cfg = OrchestratorConfig::new(".");

        assert_eq!(cfg.role(Role::Explore).model, "anthropic:claude-haiku-4-5");
        assert!(!cfg.role(Role::Code).system_prompt.is_empty());
    }

    #[test]
    fn model_override_wins_and_keeps_the_prompt() {
        let cfg = OrchestratorConfig::new(".").with_model(Role::Code, "openai:gpt-5.1");
        let code = cfg.role(Role::Code);

        assert_eq!(code.model, "openai:gpt-5.1");
        assert_eq!(code.system_prompt, crate::prompts::for_role(Role::Code));
        // Other roles are untouched.
        assert_eq!(cfg.role(Role::Explore).model, "anthropic:claude-haiku-4-5");
    }

    #[test]
    fn every_role_has_a_usable_default() {
        for role in Role::ALL {
            let cfg = RoleConfig::defaults_for(role);
            assert!(!cfg.model.is_empty(), "{role} has no model");
            assert!(!cfg.system_prompt.is_empty(), "{role} has no prompt");
        }
    }
}
