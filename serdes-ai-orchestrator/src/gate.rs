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
    /// Whether the change is clean: no leftover debris, no unrelated edits, and
    /// consistent with the surrounding code.
    pub clean: bool,
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
    /// All three axes must hold. Work that is correct but incomplete has not
    /// been done; work that is complete but broken does not function; and work
    /// that is both but leaves debug output, dead code or unrelated edits behind
    /// is not something a reviewer would accept.
    pub fn passes(&self) -> bool {
        self.complete && self.correct && self.clean
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
                "clean": {
                    "type": "boolean",
                    "description": "Is the change clean? No leftover debug output, commented-out \
                                    code or dead code; no edits unrelated to the request; and \
                                    consistent with the conventions of the surrounding code."
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
            "required": ["complete", "correct", "clean"]
        })
    }
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

/// Build the prompt every verifier receives.
///
/// Each verifier is given the same complete job rather than a slice of it:
/// splitting the question across specialists means no single verifier ever
/// answers "is this done", and the tally aggregates partial opinions instead of
/// independent judgements. Independence comes from the models differing, not
/// from the questions differing.
pub fn verifier_prompt(request: &str, plan: &Plan, evidence: &Evidence) -> String {
    let mut out = String::new();

    out.push_str(
        "Decide whether the work below was successfully completed, and whether it \
         is clean.\n\n",
    );

    out.push_str("# What the user originally asked for\n\n");
    let _ = writeln!(out, "{request}");

    out.push_str("\n# The plan that was approved\n\n");
    let _ = writeln!(out, "{plan}");

    out.push_str("\n# Evidence of what actually happened\n\n");
    out.push_str(&evidence.render());

    out.push_str(
        "\n# Your task\n\n\
         Judge three things independently.\n\n\
         - complete: does the work satisfy BOTH the original request and every step \
         of the approved plan? A plan followed to the letter that misses what the \
         user actually asked for is not complete.\n\
         - correct: does what was written actually work? Read the changed code and \
         run whatever you need to.\n\
         - clean: is the change free of leftover debug output, commented-out code, \
         dead code and edits unrelated to the request, and does it follow the \
         conventions of the code around it?\n\n\
         You have read and shell access. Verify independently rather than taking \
         anything on trust: if the plan says a test was added, find it and run it.\n\n\
         The evidence above is what actually happened. Any summary written by the \
         agents that did the work is a claim, not evidence.\n\nCall ",
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
        judgement(complete, correct, true)
    }

    fn judgement(complete: bool, correct: bool, clean: bool) -> Verdict {
        Verdict {
            complete,
            correct,
            clean,
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
    fn a_verdict_passes_only_when_all_three_axes_hold() {
        assert!(judgement(true, true, true).passes());
        assert!(!judgement(true, false, true).passes());
        assert!(!judgement(false, true, true).passes());
        assert!(
            !judgement(true, true, false).passes(),
            "working but messy work must not pass"
        );
        assert!(!judgement(false, false, false).passes());
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
    fn the_verifier_prompt_carries_the_request_the_plan_and_the_evidence() {
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

        let prompt = verifier_prompt("I want a widget please", &plan, &evidence);

        // The original request matters on its own: a plan can be followed
        // faithfully and still miss what was asked for.
        assert!(prompt.contains("I want a widget please"));
        assert!(prompt.contains("Add a widget"));
        assert!(prompt.contains("create widget.rs"));
        assert!(prompt.contains("+fn widget() {}"));
        assert!(prompt.contains(VERDICT_TOOL));
        assert!(prompt.contains("is a claim, not evidence"));
    }

    #[test]
    fn the_verifier_prompt_asks_for_all_three_axes() {
        let plan = Plan {
            summary: "s".to_string(),
            steps: vec![PlanStep {
                description: "d".to_string(),
                files: vec![],
            }],
            verification: vec![],
            concerns: vec![],
        };
        let evidence = Evidence {
            diff: None,
            changed_files: vec![],
            tests: None,
            gaps: vec![],
        };

        let prompt = verifier_prompt("req", &plan, &evidence);

        assert!(prompt.contains("complete:"));
        assert!(prompt.contains("correct:"));
        assert!(prompt.contains("clean:"));
    }

    #[test]
    fn verdict_deserializes_with_optional_fields_absent() {
        // findings and summary are optional; models routinely omit empty ones.
        let parsed: Verdict =
            serde_json::from_str(r#"{"complete":true,"correct":false,"clean":true}"#).unwrap();

        assert!(parsed.complete);
        assert!(!parsed.correct);
        assert!(parsed.clean);
        assert!(parsed.findings.is_empty());
        assert!(parsed.summary.is_empty());
    }

    #[test]
    fn a_verdict_missing_an_axis_is_rejected() {
        // All three questions must be answered. Defaulting a missing axis would
        // either silently reject good work or silently accept unjudged work; a
        // parse failure instead drops the vote, which the tally counts against
        // the quorum.
        for partial in [
            r#"{"correct":true,"clean":true}"#,
            r#"{"complete":true,"clean":true}"#,
            r#"{"complete":true,"correct":true}"#,
        ] {
            assert!(
                serde_json::from_str::<Verdict>(partial).is_err(),
                "should have been rejected: {partial}"
            );
        }
    }
}
