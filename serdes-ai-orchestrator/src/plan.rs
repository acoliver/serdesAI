//! The plan a user approves before any code is written.

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One step of an implementation plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStep {
    /// What this step changes.
    pub description: String,
    /// Files the step is expected to touch.
    #[serde(default)]
    pub files: Vec<String>,
}

/// A planner's proposed implementation.
///
/// Deserialized straight out of the planner agent via
/// `AgentBuilder::output_type`, so the plan is structured data the gate can
/// later check against rather than prose to be re-parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// One-line statement of what will be done.
    pub summary: String,
    /// The steps, in the order they should be carried out.
    pub steps: Vec<PlanStep>,
    /// How the work will be shown to have worked — commands or tests to run.
    #[serde(default)]
    pub verification: Vec<String>,
    /// Anything ambiguous or ill-advised the planner wants to flag.
    ///
    /// Surfaced to the user at approval time: a planner that spotted a problem
    /// should not have it buried.
    #[serde(default)]
    pub concerns: Vec<String>,
}

/// The tool a planner calls to submit its plan.
pub const PLAN_TOOL: &str = "submit_plan";

impl Plan {
    /// Whether the plan says anything actionable.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// JSON schema for the plan submission tool.
    ///
    /// Structured output goes through a tool rather than free JSON text:
    /// `output_type` installs a schema whose `mode` is `Json` and which never
    /// reports a tool name, so a model that answers with a tool call is not
    /// recognised as producing output. Tool-mode output is also what real
    /// providers handle most reliably.
    pub fn schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "One line stating what will be done."
                },
                "steps": {
                    "type": "array",
                    "description": "The steps in the order they should be carried out.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": {"type": "string"},
                            "files": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Files this step is expected to touch."
                            }
                        },
                        "required": ["description"]
                    }
                },
                "verification": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Commands or tests that will show the work succeeded."
                },
                "concerns": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Anything ambiguous or ill-advised worth flagging to the user."
                }
            },
            "required": ["summary", "steps"]
        })
    }
}

impl fmt::Display for Plan {
    /// Render the plan for a human to read at approval time.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.summary)?;

        if !self.steps.is_empty() {
            writeln!(f)?;
            for (i, step) in self.steps.iter().enumerate() {
                writeln!(f, "{}. {}", i + 1, step.description)?;
                if !step.files.is_empty() {
                    writeln!(f, "   files: {}", step.files.join(", "))?;
                }
            }
        }

        if !self.verification.is_empty() {
            writeln!(f, "\nVerification:")?;
            for check in &self.verification {
                writeln!(f, "  - {check}")?;
            }
        }

        if !self.concerns.is_empty() {
            writeln!(f, "\nConcerns:")?;
            for concern in &self.concerns {
                writeln!(f, "  - {concern}")?;
            }
        }

        Ok(())
    }
}

/// What the user decided about a proposed plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    /// Proceed with the plan as written.
    Approve,
    /// Send it back to the planner with this feedback.
    Reject {
        /// What to change.
        feedback: String,
    },
    /// Abandon the run.
    Cancel,
}

/// Decides whether a plan may proceed.
///
/// The seam that keeps this crate free of any UI: the CLI implements it over its
/// existing confirmation prompt, and tests implement it with a canned answer.
#[async_trait]
pub trait PlanApprover: Send + Sync {
    /// Present `plan` and return the decision.
    async fn approve(&self, plan: &Plan) -> ApprovalDecision;
}

/// An approver that always approves. Useful in tests and for unattended runs.
#[derive(Debug, Default, Clone, Copy)]
pub struct AutoApprove;

#[async_trait]
impl PlanApprover for AutoApprove {
    async fn approve(&self, _plan: &Plan) -> ApprovalDecision {
        ApprovalDecision::Approve
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        Plan {
            summary: "Add a greeting".to_string(),
            steps: vec![PlanStep {
                description: "Create greeting.txt".to_string(),
                files: vec!["greeting.txt".to_string()],
            }],
            verification: vec!["cat greeting.txt".to_string()],
            concerns: vec![],
        }
    }

    #[test]
    fn renders_summary_steps_and_verification() {
        let rendered = plan().to_string();

        assert!(rendered.contains("Add a greeting"));
        assert!(rendered.contains("1. Create greeting.txt"));
        assert!(rendered.contains("files: greeting.txt"));
        assert!(rendered.contains("cat greeting.txt"));
    }

    #[test]
    fn concerns_are_shown_when_present() {
        // A planner that flagged a problem must not have it hidden at the moment
        // the user is deciding whether to approve.
        let mut p = plan();
        p.concerns = vec!["the requirement is ambiguous".to_string()];

        let rendered = p.to_string();

        assert!(rendered.contains("Concerns:"));
        assert!(rendered.contains("the requirement is ambiguous"));
    }

    #[test]
    fn empty_sections_are_omitted() {
        let mut p = plan();
        p.verification.clear();

        let rendered = p.to_string();

        assert!(!rendered.contains("Verification:"));
        assert!(!rendered.contains("Concerns:"));
    }

    #[test]
    fn a_plan_with_no_steps_is_empty() {
        let mut p = plan();
        p.steps.clear();

        assert!(p.is_empty());
        assert!(!plan().is_empty());
    }

    #[test]
    fn deserializes_with_optional_fields_absent() {
        // Models omit empty arrays; the plan must still parse.
        let json = r#"{"summary":"do it","steps":[{"description":"step one"}]}"#;

        let parsed: Plan = serde_json::from_str(json).unwrap();

        assert_eq!(parsed.summary, "do it");
        assert_eq!(parsed.steps[0].files, Vec::<String>::new());
        assert!(parsed.verification.is_empty());
        assert!(parsed.concerns.is_empty());
    }

    #[tokio::test]
    async fn auto_approve_approves() {
        assert_eq!(
            AutoApprove.approve(&plan()).await,
            ApprovalDecision::Approve
        );
    }
}
