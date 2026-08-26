//! Workflow Mode end to end: plan, approval, execution, and the quorum gate.

mod support;

use std::fs;
use std::sync::Arc;

use serdes_ai_orchestrator::config::{GateConfig, OrchestratorConfig};
use serdes_ai_orchestrator::events::OrchestratorEvent;
use serdes_ai_orchestrator::orchestrator::Orchestrator;
use serdes_ai_orchestrator::role::Role;
use tokio_util::sync::CancellationToken;

use support::{ScriptedFactory, Turn, always_approve, git_commit_all, git_init, temp_root};

/// A verifier turn casting the given vote.
fn vote(complete: bool, correct: bool, note: &str) -> Vec<Turn> {
    graded(complete, correct, true, note)
}

/// A verifier turn casting a vote on all three axes.
fn graded(complete: bool, correct: bool, clean: bool, note: &str) -> Vec<Turn> {
    vec![Turn::Tool {
        name: "submit_verdict",
        args: serde_json::json!({
            "complete": complete,
            "correct": correct,
            "clean": clean,
            "summary": note,
            "findings": if complete && correct && clean {
                serde_json::json!([])
            } else {
                serde_json::json!([{
                    "severity": "critical",
                    "file": "widget.rs",
                    "description": note
                }])
            }
        }),
    }]
}

/// A planner turn returning a one-step plan.
fn plan_turn() -> Vec<Turn> {
    vec![Turn::Tool {
        name: "submit_plan",
        args: serde_json::json!({
            "summary": "create widget.rs",
            "steps": [{"description": "create widget.rs", "files": ["widget.rs"]}],
            "verification": ["test -f widget.rs"]
        }),
    }]
}

/// An orchestrator that delegates once, and a code agent that writes the file.
fn working_scripts(factory: ScriptedFactory) -> ScriptedFactory {
    factory
        .script(
            Role::Orchestrator,
            vec![
                Turn::Tool {
                    name: "spawn_agent",
                    args: serde_json::json!({"role": "code", "task": "create widget.rs"}),
                },
                Turn::Text("widget.rs created".to_string()),
            ],
        )
        .script(
            Role::Code,
            vec![
                Turn::Tool {
                    name: "write_file",
                    args: serde_json::json!({"path": "widget.rs", "content": "fn widget() {}\n"}),
                },
                Turn::Text("done".to_string()),
            ],
        )
        .script(Role::Planner, plan_turn())
}

fn repo() -> std::path::PathBuf {
    let root = temp_root();
    git_init(&root);
    fs::write(root.join("README.md"), "start\n").unwrap();
    git_commit_all(&root, "base");
    root
}

/// A gate with `n` distinct verifier models.
fn models(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("provider{i}:model{i}")).collect()
}

fn orchestrator(
    root: &std::path::Path,
    factory: ScriptedFactory,
    gate: GateConfig,
) -> Orchestrator {
    Orchestrator::with_factory(
        OrchestratorConfig::new(root).with_gate(gate),
        Arc::new(factory),
    )
    .unwrap()
}

#[tokio::test]
async fn work_accepted_by_a_majority_passes_the_gate() {
    let root = repo();
    let factory = working_scripts(ScriptedFactory::new()).script_each(
        Role::Verifier,
        vec![
            vote(true, true, "looks right"),
            vote(true, true, "agreed"),
            vote(false, false, "unconvinced"),
        ],
    );

    let outcome = orchestrator(&root, factory, GateConfig::default())
        .run_workflow("make a widget", &always_approve(), CancellationToken::new())
        .await
        .expect("workflow failed");

    assert!(outcome.accepted, "2 of 3 should pass");
    assert_eq!(outcome.rounds, 1);
    assert!(
        root.join("widget.rs").exists(),
        "the work should have happened"
    );
}

#[tokio::test]
async fn a_minority_verdict_does_not_pass() {
    let root = repo();
    let factory = working_scripts(ScriptedFactory::new()).script_each(
        Role::Verifier,
        vec![
            vote(true, true, "fine"),
            vote(false, false, "incomplete"),
            vote(false, true, "still incomplete"),
        ],
    );

    let outcome = orchestrator(
        &root,
        factory,
        GateConfig {
            max_rounds: 1,
            ..Default::default()
        },
    )
    .run_workflow("make a widget", &always_approve(), CancellationToken::new())
    .await
    .expect("workflow errored");

    assert!(!outcome.accepted, "1 of 3 must not pass");
    let gate = outcome.gate.expect("no gate outcome");
    assert_eq!(gate.passed, 1);
    assert_eq!(gate.total(), 3);
}

#[tokio::test]
async fn a_failing_gate_sends_the_orchestrator_back_around() {
    let root = repo();
    // Round 1 rejects, round 2 accepts: six verifier builds in total.
    let factory = working_scripts(ScriptedFactory::new()).script_each(
        Role::Verifier,
        vec![
            vote(false, false, "missing"),
            vote(false, false, "missing"),
            vote(false, false, "missing"),
            vote(true, true, "fixed"),
            vote(true, true, "fixed"),
            vote(true, true, "fixed"),
        ],
    );

    let outcome = orchestrator(&root, factory, GateConfig::default())
        .run_workflow("make a widget", &always_approve(), CancellationToken::new())
        .await
        .expect("workflow errored");

    assert!(outcome.accepted);
    assert_eq!(outcome.rounds, 2, "should have taken a second round");
}

#[tokio::test]
async fn a_gate_that_never_passes_stops_at_the_round_cap() {
    let root = repo();
    let factory = working_scripts(ScriptedFactory::new()).script_each(
        Role::Verifier,
        vec![vote(false, false, "never good enough")],
    );

    let outcome = orchestrator(
        &root,
        factory,
        GateConfig {
            max_rounds: 2,
            ..Default::default()
        },
    )
    .run_workflow("make a widget", &always_approve(), CancellationToken::new())
    .await
    .expect("workflow errored");

    assert!(!outcome.accepted);
    assert_eq!(outcome.rounds, 2, "must stop at the cap, not loop forever");

    // The user needs the findings, not just a failure.
    let gate = outcome.gate.expect("no gate outcome");
    assert!(!gate.dissent().is_empty());
    assert!(gate.feedback().contains("never good enough"));
}

#[tokio::test]
async fn verifiers_are_shown_the_real_diff_not_the_orchestrators_claims() {
    // The design's central risk: a gate reading self-reports rubber-stamps. The
    // orchestrator here claims success while the code agent writes nothing, so
    // the only way to tell is the evidence.
    let root = repo();

    let factory = ScriptedFactory::new()
        .script(Role::Planner, plan_turn())
        .script(
            Role::Orchestrator,
            vec![Turn::Text(
                "I created widget.rs exactly as planned. All done.".to_string(),
            )],
        )
        .script_each(Role::Verifier, vec![vote(true, true, "trusting")]);

    let orch = orchestrator(
        &root,
        factory,
        GateConfig {
            max_rounds: 1,
            ..Default::default()
        },
    );
    let mut rx = orch.subscribe();

    orch.run_workflow("make a widget", &always_approve(), CancellationToken::new())
        .await
        .expect("workflow errored");

    // Nothing was actually written.
    assert!(!root.join("widget.rs").exists());

    // The verifiers must have been spawned, and the run must have reached the
    // gate rather than accepting the orchestrator's word on its own.
    let mut verifier_spawns = 0;
    let mut gate_ran = false;
    while let Ok(event) = rx.try_recv() {
        match event {
            OrchestratorEvent::AgentSpawned {
                role: Role::Verifier,
                ..
            } => verifier_spawns += 1,
            OrchestratorEvent::GateResult { .. } => gate_ran = true,
            _ => {}
        }
    }

    assert_eq!(
        verifier_spawns, 3,
        "every verifier should have been consulted"
    );
    assert!(
        gate_ran,
        "the gate must run even when the orchestrator claims success"
    );
}

#[tokio::test]
async fn every_verifier_votes_and_the_round_is_published() {
    let root = repo();
    let factory = working_scripts(ScriptedFactory::new())
        .script_each(Role::Verifier, vec![vote(true, true, "ok")]);

    let orch = orchestrator(
        &root,
        factory,
        GateConfig {
            verifier_models: models(5),
            ..Default::default()
        },
    );
    let mut rx = orch.subscribe();

    orch.run_workflow("make a widget", &always_approve(), CancellationToken::new())
        .await
        .expect("workflow errored");

    let mut verdicts = 0;
    let mut round_start = None;
    let mut result = None;
    while let Ok(event) = rx.try_recv() {
        match event {
            OrchestratorEvent::GateVerdict { .. } => verdicts += 1,
            OrchestratorEvent::GateRoundStart { verifiers, .. } => round_start = Some(verifiers),
            OrchestratorEvent::GateResult {
                passed,
                total,
                quorum_met,
                ..
            } => result = Some((passed, total, quorum_met)),
            _ => {}
        }
    }

    assert_eq!(
        verdicts, 5,
        "a configurable verifier count must be honoured"
    );
    assert_eq!(round_start, Some(5));
    assert_eq!(result, Some((5, 5, true)));
}

#[tokio::test]
async fn a_rejected_plan_never_reaches_the_gate() {
    use async_trait::async_trait;
    use serdes_ai_orchestrator::plan::{ApprovalDecision, Plan, PlanApprover};

    struct Decline;
    #[async_trait]
    impl PlanApprover for Decline {
        async fn approve(&self, _plan: &Plan) -> ApprovalDecision {
            ApprovalDecision::Cancel
        }
    }

    let root = repo();
    let factory = working_scripts(ScriptedFactory::new());
    let orch = orchestrator(&root, factory, GateConfig::default());

    let result = orch
        .run_workflow("make a widget", &Decline, CancellationToken::new())
        .await;

    assert!(result.is_err(), "cancelling approval must abandon the run");
    assert!(
        !root.join("widget.rs").exists(),
        "no work may happen before the plan is approved"
    );
}

#[tokio::test]
async fn each_verifier_runs_on_its_own_model() {
    // The gate's independence comes from model diversity, so every configured
    // model must actually be used exactly once per round.
    let root = repo();
    let factory = Arc::new(
        working_scripts(ScriptedFactory::new())
            .script_each(Role::Verifier, vec![vote(true, true, "ok")]),
    );

    let orch = Orchestrator::with_factory(
        OrchestratorConfig::new(&root).with_gate(GateConfig {
            verifier_models: models(4),
            max_rounds: 1,
            ..Default::default()
        }),
        factory.clone(),
    )
    .unwrap();

    orch.run_workflow("make a widget", &always_approve(), CancellationToken::new())
        .await
        .expect("workflow errored");

    let asked = factory.specs_for(Role::Verifier);
    let mut unique = asked.clone();
    unique.sort();
    unique.dedup();

    assert_eq!(asked.len(), 4, "every verifier should have been built");
    assert_eq!(
        unique.len(),
        4,
        "each verifier must use a distinct model, got {asked:?}"
    );
}

#[tokio::test]
async fn work_that_is_unclean_does_not_pass() {
    // Complete and correct but messy must still be rejected.
    let root = repo();
    let factory = working_scripts(ScriptedFactory::new()).script_each(
        Role::Verifier,
        vec![graded(true, true, false, "leftover debug output")],
    );

    let outcome = orchestrator(
        &root,
        factory,
        GateConfig {
            max_rounds: 1,
            ..Default::default()
        },
    )
    .run_workflow("make a widget", &always_approve(), CancellationToken::new())
    .await
    .expect("workflow errored");

    assert!(!outcome.accepted, "unclean work must not pass the gate");
    let gate = outcome.gate.expect("no gate outcome");
    assert_eq!(gate.passed, 0);
    assert!(gate.feedback().contains("leftover debug output"));
}
