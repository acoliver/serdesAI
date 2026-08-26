//! Feasibility spike for the `spawn_agent` tool.
//!
//! The plan flagged one open risk: `AgentBuilder::tool_fn_async` requires
//! `F: Fn(&RunContext<Deps>, Args) -> Fut + Send + Sync + 'static` with
//! `Fut: Future + Send + 'static`. The future may therefore not borrow from the
//! `RunContext`, and the closure is `Fn` (not `FnOnce`), so every captured
//! handle must be cloned per call.
//!
//! This module proves that a *nested agent run* can live inside such a closure —
//! the mechanism the whole tool-driven orchestration design rests on. It is
//! test-only and exists to fail loudly at compile time if that stops holding.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Deserialize;
use serdes_ai_agent::{AgentBuilder, RunContext};
use serdes_ai_core::{FinishReason, ModelResponse, ModelResponsePart};
use serdes_ai_models::FunctionModel;
use serdes_ai_tools::{ToolError, ToolReturn};

use crate::role::Role;

#[derive(Debug, Deserialize)]
struct SpawnArgs {
    role: String,
    task: String,
}

/// Stands in for the real `AgentRegistry`: something `Arc`-held that the closure
/// must reach through on every invocation, and which itself runs an agent.
struct Registry {
    /// Counts nested runs so a test can prove the inner agent really executed.
    runs: AtomicUsize,
}

impl Registry {
    fn new() -> Self {
        Self {
            runs: AtomicUsize::new(0),
        }
    }

    async fn run_role(&self, role: Role, task: &str) -> Result<String, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);

        let reply = format!("{role} handled: {task}");
        let agent = AgentBuilder::<(), String>::new(FunctionModel::constant_text(reply))
            .system_prompt("subagent")
            .build();

        agent
            .run(task.to_string(), ())
            .await
            .map(|r| r.output)
            .map_err(|e| e.to_string())
    }
}

/// A model that calls `spawn_agent` once, then answers with text.
///
/// Without the second phase the agent loop would call the tool forever.
fn orchestrator_model() -> FunctionModel {
    FunctionModel::new(move |requests, _settings| {
        let already_delegated = requests
            .iter()
            .any(|req| format!("{req:?}").contains("spawn_agent"));

        if already_delegated {
            ModelResponse::text("delegation complete").with_finish_reason(FinishReason::Stop)
        } else {
            ModelResponse::with_parts(vec![ModelResponsePart::tool_call(
                "spawn_agent",
                serde_json::json!({ "role": "code", "task": "add a function" }),
            )])
            .with_finish_reason(FinishReason::ToolCall)
        }
    })
}

/// Build an agent carrying a `spawn_agent` tool that runs a nested agent.
///
/// This compiling at all is the primary result the spike is after.
fn build_orchestrator(registry: Arc<Registry>) -> serdes_ai_agent::Agent<(), String> {
    AgentBuilder::<(), String>::new(orchestrator_model())
        .system_prompt("orchestrator")
        .tool_fn_async(
            "spawn_agent",
            "Delegate a task to a subagent",
            move |_ctx: &RunContext<()>, args: SpawnArgs| {
                // Cloned per call: the closure is `Fn`, and the returned future
                // must be 'static, so nothing may be borrowed from the enclosing
                // scope or from `_ctx`.
                let registry = Arc::clone(&registry);
                async move {
                    let role: Role = args.role.parse().map_err(|e: crate::role::UnknownRole| {
                        ToolError::execution_failed(e.to_string())
                    })?;

                    let output = registry
                        .run_role(role, &args.task)
                        .await
                        .map_err(ToolError::execution_failed)?;

                    Ok(ToolReturn::text(output))
                }
            },
        )
        .build()
}

#[tokio::test]
async fn nested_agent_runs_directly() {
    // Guards the inner half on its own, so a failure below can be attributed to
    // the closure rather than to the nested run itself.
    let registry = Registry::new();
    let out = registry.run_role(Role::Code, "add a function").await;

    assert!(out.is_ok(), "nested run failed: {out:?}");
    assert!(out.unwrap().contains("code handled"));
    assert_eq!(registry.runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn spawn_agent_closure_can_run_a_nested_agent() {
    let registry = Arc::new(Registry::new());
    let agent = build_orchestrator(Arc::clone(&registry));

    let result = agent.run("do the thing".to_string(), ()).await;

    assert!(result.is_ok(), "orchestrator run failed: {result:?}");
    assert_eq!(
        registry.runs.load(Ordering::SeqCst),
        1,
        "the spawn_agent tool should have run exactly one nested agent"
    );
}
