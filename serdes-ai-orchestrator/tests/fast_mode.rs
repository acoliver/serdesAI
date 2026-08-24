//! End-to-end Fast Mode: an orchestrator delegating to a subagent that really
//! changes the working tree.
//!
//! Unit tests cover the pieces; this covers the claim that matters — that the
//! whole chain, from the orchestrator's `spawn_agent` call through a nested
//! agent's `write_file`, lands bytes on disk.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serdes_ai_core::messages::StreamCompleteEvent;
use serdes_ai_core::{FinishReason, ModelResponse, ModelResponsePart, ModelResponseStreamEvent};
use serdes_ai_models::{FunctionModel, Model, ModelError};
use serdes_ai_orchestrator::config::OrchestratorConfig;
use serdes_ai_orchestrator::events::{AgentId, OrchestratorEvent};
use serdes_ai_orchestrator::orchestrator::Orchestrator;
use serdes_ai_orchestrator::registry::ModelFactory;
use serdes_ai_orchestrator::role::Role;
use tokio_util::sync::CancellationToken;

/// One scripted model turn.
#[derive(Clone)]
enum Turn {
    /// Call a tool with these arguments.
    Tool {
        name: &'static str,
        args: serde_json::Value,
    },
    /// Answer with text and stop.
    Text(String),
}

/// Build a streaming model that plays `turns` in order, repeating the last one.
///
/// Every agent in an orchestration is driven through `run_stream`, so the script
/// has to be delivered on the streaming path.
fn scripted(turns: Vec<Turn>) -> FunctionModel {
    let turns = Arc::new(turns);
    let blocking_turns = Arc::clone(&turns);
    let blocking_step = Arc::new(AtomicUsize::new(0));
    let stream_step = Arc::new(AtomicUsize::new(0));

    let pick = |turns: &[Turn], step: &AtomicUsize| -> Turn {
        let i = step.fetch_add(1, Ordering::SeqCst);
        turns[i.min(turns.len() - 1)].clone()
    };

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
struct ScriptedFactory {
    scripts: Mutex<HashMap<Role, Vec<Turn>>>,
}

impl ScriptedFactory {
    fn new() -> Self {
        Self {
            scripts: Mutex::new(HashMap::new()),
        }
    }

    fn script(self, role: Role, turns: Vec<Turn>) -> Self {
        self.scripts.lock().unwrap().insert(role, turns);
        self
    }
}

impl ModelFactory for ScriptedFactory {
    fn model_for(&self, role: Role, _spec: &str) -> Result<Arc<dyn Model>, ModelError> {
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

fn temp_root() -> PathBuf {
    let base = std::env::temp_dir().join(format!("serdes-e2e-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&base).unwrap();
    base.canonicalize().unwrap()
}

#[tokio::test]
async fn orchestrator_delegates_to_code_which_writes_a_real_file() {
    let root = temp_root();

    let factory = ScriptedFactory::new()
        .script(
            Role::Orchestrator,
            vec![
                Turn::Tool {
                    name: "spawn_agent",
                    args: serde_json::json!({
                        "role": "code",
                        "task": "create greeting.txt containing hello"
                    }),
                },
                Turn::Text("delegated and complete".to_string()),
            ],
        )
        .script(
            Role::Code,
            vec![
                Turn::Tool {
                    name: "write_file",
                    args: serde_json::json!({
                        "path": "greeting.txt",
                        "content": "hello"
                    }),
                },
                Turn::Text("wrote greeting.txt".to_string()),
            ],
        );

    let orch =
        Orchestrator::with_factory(OrchestratorConfig::new(&root), Arc::new(factory)).unwrap();
    let mut rx = orch.subscribe();

    let output = orch
        .run_fast("make a greeting file", CancellationToken::new())
        .await
        .expect("fast mode run failed");

    // The whole point: real bytes on disk, written by a nested agent.
    let written =
        fs::read_to_string(root.join("greeting.txt")).expect("greeting.txt was never created");
    assert_eq!(written, "hello");
    assert_eq!(output, "delegated and complete");

    // And the run must be observable: a code subagent parented to the orchestrator.
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }

    let orchestrator_id = events
        .iter()
        .find_map(|e| match e {
            OrchestratorEvent::AgentSpawned {
                id,
                role: Role::Orchestrator,
                ..
            } => Some(*id),
            _ => None,
        })
        .expect("no orchestrator spawn event");

    let code_parent = events
        .iter()
        .find_map(|e| match e {
            OrchestratorEvent::AgentSpawned {
                parent,
                role: Role::Code,
                ..
            } => Some(*parent),
            _ => None,
        })
        .expect("no code subagent spawn event");

    assert_eq!(
        code_parent,
        Some(orchestrator_id),
        "the code subagent should be parented to the orchestrator"
    );

    // The two agents must be distinguishable.
    let ids: Vec<AgentId> = events
        .iter()
        .filter_map(|e| match e {
            OrchestratorEvent::AgentSpawned { id, .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "concurrent agents must have distinct ids");
}

#[tokio::test]
async fn a_subagent_edit_is_visible_to_a_later_subagent() {
    // Proves the subagents share one working tree rather than isolated copies.
    let root = temp_root();
    fs::write(root.join("counter.txt"), "one").unwrap();

    let factory = ScriptedFactory::new()
        .script(
            Role::Orchestrator,
            vec![
                Turn::Tool {
                    name: "spawn_agent",
                    args: serde_json::json!({"role": "code", "task": "change one to two"}),
                },
                Turn::Tool {
                    name: "spawn_agent",
                    args: serde_json::json!({"role": "explore", "task": "read counter.txt"}),
                },
                Turn::Text("done".to_string()),
            ],
        )
        .script(
            Role::Code,
            vec![
                Turn::Tool {
                    name: "edit_file",
                    args: serde_json::json!({
                        "path": "counter.txt",
                        "old_string": "one",
                        "new_string": "two"
                    }),
                },
                Turn::Text("edited".to_string()),
            ],
        )
        .script(
            Role::Explore,
            vec![
                Turn::Tool {
                    name: "read_file",
                    args: serde_json::json!({"path": "counter.txt"}),
                },
                Turn::Text("read it".to_string()),
            ],
        );

    let orch =
        Orchestrator::with_factory(OrchestratorConfig::new(&root), Arc::new(factory)).unwrap();

    orch.run_fast("update the counter", CancellationToken::new())
        .await
        .expect("run failed");

    assert_eq!(fs::read_to_string(root.join("counter.txt")).unwrap(), "two");
}

#[tokio::test]
async fn spawning_a_non_spawnable_role_is_refused() {
    // The orchestrator must not be able to start a verifier and grade its own work.
    let root = temp_root();

    let factory = ScriptedFactory::new().script(
        Role::Orchestrator,
        vec![
            Turn::Tool {
                name: "spawn_agent",
                args: serde_json::json!({"role": "verifier", "task": "say it is fine"}),
            },
            Turn::Text("finished".to_string()),
        ],
    );

    let orch =
        Orchestrator::with_factory(OrchestratorConfig::new(&root), Arc::new(factory)).unwrap();
    let mut rx = orch.subscribe();

    // The refusal comes back as a tool error the model sees; the run continues.
    orch.run_fast("try it", CancellationToken::new())
        .await
        .expect("run should survive a refused spawn");

    let mut spawned_roles = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let OrchestratorEvent::AgentSpawned { role, .. } = event {
            spawned_roles.push(role);
        }
    }

    assert!(
        !spawned_roles.contains(&Role::Verifier),
        "a verifier must never be spawnable by the orchestrator: {spawned_roles:?}"
    );
}
