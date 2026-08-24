//! Shared scaffolding for the integration tests.
//!
//! Lets a test script exactly what each role's model does, so whole
//! orchestrations run deterministically with no network.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serdes_ai_core::messages::StreamCompleteEvent;
use serdes_ai_core::{FinishReason, ModelResponse, ModelResponsePart, ModelResponseStreamEvent};
use serdes_ai_models::{FunctionModel, Model, ModelError};
use serdes_ai_orchestrator::plan::AutoApprove;
use serdes_ai_orchestrator::registry::ModelFactory;
use serdes_ai_orchestrator::role::Role;

/// One scripted model turn.
#[derive(Clone)]
pub enum Turn {
    /// Call a tool with these arguments.
    ///
    /// Structured-output agents are driven by calling `final_result` with the
    /// object the agent should return.
    Tool {
        name: &'static str,
        args: serde_json::Value,
    },
    /// Answer with text and stop.
    Text(String),
}

/// Build a streaming model that plays `turns` in order, repeating the last one.
///
/// Both paths are supplied: subagents are driven through `run_stream`, while
/// structured-output agents (the planner, verifiers) go through the blocking
/// path.
pub fn scripted(turns: Vec<Turn>) -> FunctionModel {
    let turns = Arc::new(turns);
    let blocking_turns = Arc::clone(&turns);
    let blocking_step = Arc::new(AtomicUsize::new(0));
    let stream_step = Arc::new(AtomicUsize::new(0));

    fn pick(turns: &[Turn], step: &AtomicUsize) -> Turn {
        let i = step.fetch_add(1, Ordering::SeqCst);
        turns[i.min(turns.len() - 1)].clone()
    }

    FunctionModel::with_both(
        move |_, _| match pick(&blocking_turns, &blocking_step) {
            Turn::Text(text) => ModelResponse::text(text).with_finish_reason(FinishReason::Stop),
            Turn::Tool { name, args } => {
                ModelResponse::with_parts(vec![ModelResponsePart::tool_call(name, args)])
                    .with_finish_reason(FinishReason::ToolCall)
            }
        },
        move |_, _| {
            let events = match pick(&turns, &stream_step) {
                Turn::Text(text) => vec![
                    Ok(ModelResponseStreamEvent::part_start(
                        0,
                        ModelResponsePart::text(""),
                    )),
                    Ok(ModelResponseStreamEvent::text_delta(0, text)),
                    Ok(ModelResponseStreamEvent::StreamComplete(
                        StreamCompleteEvent::new(FinishReason::Stop),
                    )),
                ],
                Turn::Tool { name, args } => vec![
                    Ok(ModelResponseStreamEvent::part_start(
                        0,
                        ModelResponsePart::tool_call(name, args),
                    )),
                    Ok(ModelResponseStreamEvent::StreamComplete(
                        StreamCompleteEvent::new(FinishReason::ToolCall),
                    )),
                ],
            };
            Box::pin(futures::stream::iter(events))
        },
    )
}

/// Hands each role its own script.
pub struct ScriptedFactory {
    scripts: Mutex<HashMap<Role, Vec<Turn>>>,
    /// Roles that were asked for, in order.
    pub requested: Mutex<Vec<Role>>,
}

impl ScriptedFactory {
    pub fn new() -> Self {
        Self {
            scripts: Mutex::new(HashMap::new()),
            requested: Mutex::new(Vec::new()),
        }
    }

    pub fn script(self, role: Role, turns: Vec<Turn>) -> Self {
        self.scripts.lock().unwrap().insert(role, turns);
        self
    }
}

impl Default for ScriptedFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelFactory for ScriptedFactory {
    fn model_for(&self, role: Role, _spec: &str) -> Result<Arc<dyn Model>, ModelError> {
        self.requested.lock().unwrap().push(role);

        let turns = self
            .scripts
            .lock()
            .unwrap()
            .get(&role)
            .cloned()
            .unwrap_or_else(|| vec![Turn::Text(format!("{role} had no script"))]);

        Ok(Arc::new(scripted(turns)))
    }
}

/// An approver that accepts whatever it is shown.
pub fn always_approve() -> AutoApprove {
    AutoApprove
}

/// A fresh directory for a test to work in.
pub fn temp_root() -> PathBuf {
    let base = std::env::temp_dir().join(format!("serdes-e2e-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).unwrap();
    // Resolve symlinks (macOS /var -> /private/var) so root-containment checks
    // compare like with like.
    base.canonicalize().unwrap()
}

/// Initialise a git repo so diff-based evidence has a baseline.
pub fn git_init(root: &std::path::Path) {
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "test@example.com"],
        vec!["config", "user.name", "Test"],
    ] {
        std::process::Command::new("git")
            .args(&args)
            .current_dir(root)
            .output()
            .expect("git command failed");
    }
}

/// Commit everything currently in `root`.
pub fn git_commit_all(root: &std::path::Path, message: &str) {
    for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", message]] {
        std::process::Command::new("git")
            .args(&args)
            .current_dir(root)
            .output()
            .expect("git command failed");
    }
}
