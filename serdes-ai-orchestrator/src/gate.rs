//! The quorum gate.
//!
//! Independent verifiers judge finished work against the plan it was meant to
//! implement and the evidence of what actually changed. Passing needs strictly
//! more than half of them.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::evidence::Evidence;
use crate::plan::Plan;

/// The tool a verifier calls to cast its vote.
pub const VERDICT_TOOL: &str = "submit_verdict";

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Blocks acceptance.
    Critical,
    /// Should be addressed.
    Major,
    /// Worth noting.
    Minor,
}

/// Something a verifier found wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// How serious it is.
    pub severity: Severity,
    /// Where it is, if it has a location.
    #[serde(default)]
    pub file: Option<String>,
    /// What is wrong.
    pub description: String,
}

/// One verifier's judgement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    /// Whether the diff covers everything the plan called for.
    pub complete: bool,
    /// Whether what was written actually works.
    pub correct: bool,
    /// What the verifier found wrong.
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// The verifier's reasoning in brief.
    #[serde(default)]
    pub summary: String,
}

impl Verdict {
    /// Whether this verifier votes to accept.
    ///
    /// Both axes must hold: work that is correct but incomplete has not been
    /// done, and work that is complete but broken does not function.
    pub fn passes(&self) -> bool {
        self.complete && self.correct
    }

    /// JSON schema for the verdict submission tool.
    pub fn schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "complete": {
                    "type": "boolean",
                    "description": "Does the diff cover everything the plan called for?"
                },
                "correct": {
                    "type": "boolean",
                    "description": "Does what was written actually work?"
                },
                "findings": {
                    "type": "array",
                    "description": "Specific problems found, each with a location where possible.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "severity": {
                                "type": "string",
                                "enum": ["critical", "major", "minor"]
                            },
                            "file": {"type": "string"},
                            "description": {"type": "string"}
                        },
                        "required": ["severity", "description"]
                    }
                },
                "summary": {
                    "type": "string",
                    "description": "Brief reasoning for the verdict."
                }
            },
            "required": ["complete", "correct"]
        })
    }
}

/// The distinct angle one verifier is asked to take.
///
/// Independent verifiers running the same prompt tend to produce the same
/// opinion, which makes a quorum theatre rather than a check. Each verifier is
/// pointed at a different way the work could be wrong; beyond the list, lenses
/// repeat.
pub const LENSES: [&str; 4] = [
    "Focus on completeness: walk the plan step by step and confirm each one is \
     actually present in the diff. Work that was silently skipped is your priority.",
    "Focus on correctness: read the changed code closely for bugs, unhandled \
     cases, and mistakes that the tests would not catch.",
    "Focus on regressions: consider what previously worked and might now be \
     broken, including callers of anything whose behaviour changed.",
    "Focus on verification: check that what the plan claimed would prove the work \
     — tests, commands — exists and actually passes. Run it yourself.",
];

/// The lens for verifier `index`.
pub fn lens_for(index: usize) -> &'static str {
    LENSES[index % LENSES.len()]
}

/// The outcome of one gate round.
#[derive(Debug, Clone)]
pub struct GateOutcome {
    /// Every verdict actually cast.
    pub verdicts: Vec<Verdict>,
    /// How many voted to accept.
    pub passed: usize,
    /// How many verifiers were asked to vote.
    pub expected: usize,
    /// Whether quorum was reached.
    pub quorum_met: bool,
}

impl GateOutcome {
    /// Tally `verdicts` from `expected` verifiers under a
    /// strictly-more-than-half rule.
    ///
    /// Quorum is measured against how many verifiers were *asked*, not how many
    /// answered. A verifier that failed did not accept the work, and letting the
    /// denominator shrink would mean two crashed verifiers could turn a single
    /// remaining vote into unanimous approval.
    pub fn tally(verdicts: Vec<Verdict>, expected: usize) -> Self {
        let passed = verdicts.iter().filter(|v| v.passes()).count();
        let expected = expected.max(verdicts.len());
        let quorum_met = crate::config::GateConfig::quorum_met(passed, expected);

        Self {
            verdicts,
            passed,
            expected,
            quorum_met,
        }
    }

    /// How many verifiers were asked to vote.
    pub fn total(&self) -> usize {
        self.expected
    }

    /// Findings from the verifiers that voted to reject, most serious first.
    ///
    /// Only dissenters' findings: a verifier that voted to accept while noting a
    /// minor point should not send the orchestrator back around.
    pub fn dissent(&self) -> Vec<&Finding> {
        let mut findings: Vec<&Finding> = self
            .verdicts
            .iter()
            .filter(|v| !v.passes())
            .flat_map(|v| v.findings.iter())
            .collect();

        findings.sort_by_key(|f| f.severity);
        findings
    }

    /// Feedback to hand back to the orchestrator for another round.
    pub fn feedback(&self) -> String {
        let mut out = String::new();

        let _ = writeln!(
            out,
            "The verification gate did not pass: {} of {} verifiers accepted the work.\n",
            self.passed,
            self.total()
        );

        let dissent = self.dissent();
        if dissent.is_empty() {
            out.push_str(
                "No specific findings were reported. Re-check the plan against what \
                 was actually changed.\n",
            );
            return out;
        }

        out.push_str("Address these findings:\n\n");

        // Deduplicate: several verifiers independently finding the same problem
        // should not read as several problems.
        let mut seen = Vec::new();
        for finding in dissent {
            let key = (finding.severity, finding.file.clone(), &finding.description);
            if seen.iter().any(|s: &(Severity, Option<String>, &String)| {
                s.0 == key.0 && s.1 == key.1 && s.2 == key.2
            }) {
                continue;
            }
            seen.push(key);

            let location = finding
                .file
                .as_deref()
                .map(|f| format!(" ({f})"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "- [{:?}]{} {}",
                finding.severity, location, finding.description
            );
        }

        out
    }
}

/// Build the prompt for one verifier.
pub fn verifier_prompt(plan: &Plan, evidence: &Evidence, lens_index: usize) -> String {
    let mut out = String::new();

    out.push_str(
        "Judge whether the following approved plan was correctly and completely implemented.\n\n",
    );
    out.push_str("# The approved plan\n\n");
    let _ = writeln!(out, "{plan}");

    out.push_str("\n# Evidence\n\n");
    out.push_str(&evidence.render());

    out.push_str("\n# Your lens\n\n");
    out.push_str(lens_for(lens_index));

    out.push_str(
        "\n\nThe evidence above is what actually happened. Any summary of the work \
         written by the agents that did it is a claim, not evidence.\n\nCall ",
    );
    out.push_str(VERDICT_TOOL);
    out.push_str(" with your judgement.");

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::PlanStep;

    fn verdict(complete: bool, correct: bool) -> Verdict {
        Verdict {
            complete,
            correct,
            findings: Vec::new(),
            summary: String::new(),
        }
    }

    fn finding(severity: Severity, description: &str) -> Finding {
        Finding {
            severity,
            file: None,
            description: description.to_string(),
        }
    }

    #[test]
    fn a_verdict_passes_only_when_complete_and_correct() {
        assert!(verdict(true, true).passes());
        assert!(!verdict(true, false).passes());
        assert!(!verdict(false, true).passes());
        assert!(!verdict(false, false).passes());
    }

    #[test]
    fn quorum_needs_a_majority() {
        let outcome = GateOutcome::tally(
            vec![
                verdict(true, true),
                verdict(true, true),
                verdict(false, true),
            ],
            3,
        );

        assert_eq!(outcome.passed, 2);
        assert_eq!(outcome.total(), 3);
        assert!(outcome.quorum_met);
    }

    #[test]
    fn a_minority_does_not_pass() {
        let outcome = GateOutcome::tally(
            vec![
                verdict(true, true),
                verdict(false, true),
                verdict(true, false),
            ],
            3,
        );

        assert_eq!(outcome.passed, 1);
        assert!(!outcome.quorum_met);
    }

    #[test]
    fn an_even_split_does_not_pass() {
        let outcome = GateOutcome::tally(
            vec![
                verdict(true, true),
                verdict(true, true),
                verdict(false, false),
                verdict(false, false),
            ],
            4,
        );

        assert_eq!(outcome.passed, 2);
        assert!(!outcome.quorum_met, "2 of 4 is not more than half");
    }

    #[test]
    fn dissent_collects_only_from_rejecting_verifiers() {
        // A passing verifier's nitpick must not send the work back.
        let mut accepted = verdict(true, true);
        accepted.findings = vec![finding(
            Severity::Minor,
            "nitpick from an accepting verifier",
        )];

        let mut rejected = verdict(false, true);
        rejected.findings = vec![finding(Severity::Critical, "the real problem")];

        let outcome = GateOutcome::tally(vec![accepted, rejected, verdict(true, true)], 3);
        let dissent = outcome.dissent();

        assert_eq!(dissent.len(), 1);
        assert_eq!(dissent[0].description, "the real problem");
    }

    #[test]
    fn dissent_is_ordered_by_severity() {
        let mut a = verdict(false, false);
        a.findings = vec![
            finding(Severity::Minor, "small"),
            finding(Severity::Critical, "big"),
            finding(Severity::Major, "medium"),
        ];

        let outcome = GateOutcome::tally(vec![a], 1);
        let order: Vec<Severity> = outcome.dissent().iter().map(|f| f.severity).collect();

        assert_eq!(
            order,
            vec![Severity::Critical, Severity::Major, Severity::Minor]
        );
    }

    #[test]
    fn feedback_deduplicates_the_same_finding_from_several_verifiers() {
        // Three verifiers spotting one bug is one bug.
        let mut a = verdict(false, false);
        a.findings = vec![finding(Severity::Critical, "same bug")];
        let mut b = verdict(false, false);
        b.findings = vec![finding(Severity::Critical, "same bug")];

        let feedback = GateOutcome::tally(vec![a, b, verdict(true, true)], 3).feedback();

        assert_eq!(feedback.matches("same bug").count(), 1, "{feedback}");
    }

    #[test]
    fn feedback_reports_the_tally() {
        let feedback = GateOutcome::tally(
            vec![
                verdict(true, true),
                verdict(false, false),
                verdict(false, false),
            ],
            3,
        )
        .feedback();

        assert!(feedback.contains("1 of 3"), "{feedback}");
    }

    #[test]
    fn feedback_says_something_useful_when_there_are_no_findings() {
        let feedback = GateOutcome::tally(vec![verdict(false, false)], 1).feedback();

        assert!(feedback.contains("No specific findings"));
    }

    #[test]
    fn a_crashed_verifier_counts_against_the_quorum() {
        // Two verifiers failed to report; the one that did accepted. That must
        // not read as unanimous approval.
        let outcome = GateOutcome::tally(vec![verdict(true, true)], 3);

        assert_eq!(outcome.passed, 1);
        assert_eq!(outcome.total(), 3);
        assert!(
            !outcome.quorum_met,
            "one surviving vote out of three asked is not a majority"
        );
    }

    #[test]
    fn lenses_differ_across_the_first_verifiers() {
        // Identical prompts would make the quorum theatre.
        assert_ne!(lens_for(0), lens_for(1));
        assert_ne!(lens_for(1), lens_for(2));
        assert_ne!(lens_for(2), lens_for(3));
    }

    #[test]
    fn lenses_wrap_for_large_verifier_counts() {
        assert_eq!(lens_for(0), lens_for(LENSES.len()));
    }

    #[test]
    fn the_verifier_prompt_carries_the_plan_the_evidence_and_the_lens() {
        let plan = Plan {
            summary: "Add a widget".to_string(),
            steps: vec![PlanStep {
                description: "create widget.rs".to_string(),
                files: vec!["widget.rs".to_string()],
            }],
            verification: vec!["cargo test".to_string()],
            concerns: vec![],
        };
        let evidence = Evidence {
            diff: Some("+fn widget() {}".to_string()),
            changed_files: vec!["widget.rs".to_string()],
            tests: None,
            gaps: vec![],
        };

        let prompt = verifier_prompt(&plan, &evidence, 0);

        assert!(prompt.contains("Add a widget"));
        assert!(prompt.contains("create widget.rs"));
        assert!(prompt.contains("+fn widget() {}"));
        assert!(prompt.contains(lens_for(0)));
        assert!(prompt.contains(VERDICT_TOOL));
        // The instruction that keeps the gate honest.
        assert!(prompt.contains("is a claim, not evidence"));
    }

    #[test]
    fn verdict_deserializes_with_optional_fields_absent() {
        let parsed: Verdict = serde_json::from_str(r#"{"complete":true,"correct":false}"#).unwrap();

        assert!(parsed.complete);
        assert!(!parsed.correct);
        assert!(parsed.findings.is_empty());
    }
}
