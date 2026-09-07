//! The planning phase: drafting a plan and putting it to a human.

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serdes_ai_orchestrator::config::OrchestratorConfig;
use serdes_ai_orchestrator::events::OrchestratorEvent;
use serdes_ai_orchestrator::orchestrator::{OrchestrationError, Orchestrator};
use serdes_ai_orchestrator::plan::{ApprovalDecision, Plan, PlanApprover};
use serdes_ai_orchestrator::role::Role;
use tokio_util::sync::CancellationToken;

use support::{ScriptedFactory, Turn};

/// An approver that plays a fixed sequence of decisions and records the plans.
struct ScriptedApprover {
    decisions: Vec<ApprovalDecision>,
    calls: AtomicUsize,
    seen: Mutex<Vec<Plan>>,
}

impl ScriptedApprover {
    fn new(decisions: Vec<ApprovalDecision>) -> Self {
        Self {
            decisions,
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl PlanApprover for ScriptedApprover {
    async fn approve(&self, plan: &Plan) -> ApprovalDecision {
        self.seen.lock().unwrap().push(plan.clone());
        let i = self.calls.fetch_add(1, Ordering::SeqCst);
        self.decisions
            .get(i)
            .cloned()
            .unwrap_or(ApprovalDecision::Approve)
    }
}

/// A planner whose structured output is `json`.
fn planner_returning(json: serde_json::Value) -> ScriptedFactory {
    ScriptedFactory::new().script(
        Role::Planner,
        vec![Turn::Tool {
            name: "submit_plan",
            args: json,
        }],
    )
}

fn a_plan(summary: &str) -> serde_json::Value {
    serde_json::json!({
        "summary": summary,
        "steps": [{"description": "do the thing", "files": ["a.rs"]}],
        "verification": ["cargo test"]
    })
}

fn orchestrator(factory: ScriptedFactory) -> Orchestrator {
    Orchestrator::with_factory(OrchestratorConfig::new("."), Arc::new(factory)).unwrap()
}

#[tokio::test]
async fn an_approved_plan_is_returned() {
    let orch = orchestrator(planner_returning(a_plan("add a feature")));
    let approver = ScriptedApprover::new(vec![ApprovalDecision::Approve]);

    let plan = orch
        .plan("add a feature", &approver, &CancellationToken::new())
        .await
        .expect("planning failed");

    assert_eq!(plan.summary, "add a feature");
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(approver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancelling_at_approval_abandons_the_run() {
    let orch = orchestrator(planner_returning(a_plan("something")));
    let approver = ScriptedApprover::new(vec![ApprovalDecision::Cancel]);

    let err = orch
        .plan("task", &approver, &CancellationToken::new())
        .await
        .unwrap_err();

    assert!(matches!(err, OrchestrationError::PlanCancelled));
}

#[tokio::test]
async fn a_rejection_sends_the_feedback_back_to_the_planner() {
    let orch = orchestrator(planner_returning(a_plan("revised")));
    let approver = ScriptedApprover::new(vec![
        ApprovalDecision::Reject {
            feedback: "use the existing helper".to_string(),
        },
        ApprovalDecision::Approve,
    ]);

    let plan = orch
        .plan("task", &approver, &CancellationToken::new())
        .await
        .expect("planning failed");

    assert_eq!(plan.summary, "revised");
    assert_eq!(
        approver.calls.load(Ordering::SeqCst),
        2,
        "the plan should have been put to the user twice"
    );
}

#[tokio::test]
async fn repeated_rejection_gives_up_at_the_cap() {
    // Without a cap a user who keeps rejecting would loop forever.
    let config = OrchestratorConfig {
        max_plan_revisions: 2,
        ..OrchestratorConfig::new(".")
    };
    let orch = Orchestrator::with_factory(
        config,
        Arc::new(planner_returning(a_plan("never good enough"))),
    )
    .unwrap();

    let approver = ScriptedApprover::new(vec![
        ApprovalDecision::Reject {
            feedback: "no".to_string(),
        },
        ApprovalDecision::Reject {
            feedback: "still no".to_string(),
        },
        ApprovalDecision::Reject {
            feedback: "no again".to_string(),
        },
        // A fourth would be accepted, but the cap must bite first.
    ]);

    let err = orch
        .plan("task", &approver, &CancellationToken::new())
        .await
        .unwrap_err();

    assert!(
        matches!(err, OrchestrationError::PlanRejected { attempts: 3 }),
        "got {err:?}"
    );
    assert_eq!(approver.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_plan_with_no_steps_is_rejected() {
    // An empty plan would sail through the gate later having asked for nothing.
    let orch = orchestrator(planner_returning(serde_json::json!({
        "summary": "nothing to do",
        "steps": []
    })));

    let err = orch
        .plan(
            "task",
            &support::always_approve(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(matches!(err, OrchestrationError::EmptyPlan), "got {err:?}");
}

#[tokio::test]
async fn planning_publishes_the_plan_and_the_decision() {
    let orch = orchestrator(planner_returning(a_plan("observable")));
    let mut rx = orch.subscribe();

    orch.plan(
        "task",
        &support::always_approve(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    let mut drafted = None;
    let mut decision = None;
    while let Ok(event) = rx.try_recv() {
        match event {
            OrchestratorEvent::PlanDrafted { plan, attempt } => drafted = Some((plan, attempt)),
            OrchestratorEvent::PlanDecision { decision: d } => decision = Some(d),
            _ => {}
        }
    }

    let (plan, attempt) = drafted.expect("no PlanDrafted event");
    assert_eq!(plan.summary, "observable");
    assert_eq!(attempt, 0);
    assert_eq!(decision, Some(ApprovalDecision::Approve));
}

#[tokio::test]
async fn an_already_cancelled_token_stops_planning() {
    let orch = orchestrator(planner_returning(a_plan("never reached")));
    let cancel = CancellationToken::new();
    cancel.cancel();

    let err = orch
        .plan("task", &support::always_approve(), &cancel)
        .await
        .unwrap_err();

    assert!(matches!(err, OrchestrationError::Run(_)), "got {err:?}");
}
