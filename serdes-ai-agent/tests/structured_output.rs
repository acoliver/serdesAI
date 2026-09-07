//! Structured output must actually be requested from the model.
//!
//! Regression coverage for a defect where neither `output_type` nor
//! `output_tool` communicated anything to the provider: the output tool was
//! never advertised and no schema was ever sent, so a model had no way to know
//! structured output was wanted. A model that guessed and called the output tool
//! anyway had the call treated as an unknown regular tool, which failed and made
//! the agent ask again — indefinitely.
//!
//! Advertising itself is covered by unit tests in `builder.rs`, where the
//! crate-private `tool_definitions()` (what actually reaches the model, as
//! opposed to the public `tools()`) is reachable.

use serde::Deserialize;
use serdes_ai_agent::AgentBuilder;
use serdes_ai_core::{FinishReason, ModelResponse, ModelResponsePart};
use serdes_ai_models::FunctionModel;

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Answer {
    value: String,
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {"value": {"type": "string"}},
        "required": ["value"]
    })
}

/// A model that always answers by calling `tool_name`.
fn calls_tool(tool_name: &'static str) -> FunctionModel {
    FunctionModel::new(move |_, _| {
        ModelResponse::with_parts(vec![ModelResponsePart::tool_call(
            tool_name,
            serde_json::json!({"value": "hello"}),
        )])
        .with_finish_reason(FinishReason::ToolCall)
    })
}

#[tokio::test]
async fn calling_the_output_tool_completes_the_run() {
    // The defect made this loop forever rather than returning.
    let agent = AgentBuilder::<(), String>::new(calls_tool("submit_answer"))
        .system_prompt("p")
        .output_tool::<Answer>("submit_answer", schema())
        .build();

    let result = agent.run("go".to_string(), ()).await;

    assert_eq!(
        result.expect("run failed").output,
        Answer {
            value: "hello".to_string()
        }
    );
}

#[test]
fn json_mode_advertises_no_tool() {
    // output_type asks for JSON natively rather than through a tool, so no tool
    // definition should appear. The schema instead travels on the request, where
    // a provider such as openai/chat.rs turns it into a response_format.
    let agent = AgentBuilder::<(), String>::new(FunctionModel::constant_text("x"))
        .system_prompt("p")
        .output_type_with_schema::<Answer>(schema())
        .build();

    let names: Vec<&str> = agent.tools().iter().map(|d| d.name.as_str()).collect();

    assert!(
        names.is_empty(),
        "json mode should advertise no tool: {names:?}"
    );
}

#[tokio::test]
async fn an_unknown_tool_call_does_not_spin_forever() {
    // A model that names a tool nobody registered used to be re-prompted
    // endlessly. UsageLimits must bound it.
    use serdes_ai_agent::UsageLimits;

    let agent = AgentBuilder::<(), String>::new(FunctionModel::new(|_, _| {
        ModelResponse::with_parts(vec![ModelResponsePart::tool_call(
            "no_such_tool",
            serde_json::json!({}),
        )])
        .with_finish_reason(FinishReason::ToolCall)
    }))
    .system_prompt("p")
    .usage_limits(UsageLimits {
        max_requests: Some(3),
        ..UsageLimits::default()
    })
    .build();

    // The point is that it terminates at all.
    let result = agent.run("go".to_string(), ()).await;

    assert!(
        result.is_err(),
        "an agent that only ever calls an unknown tool should stop, not succeed"
    );
}
