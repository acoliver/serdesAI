//! Smoke test for the wire-accurate Open Responses rig adopted under
//! `tests/rig`: the most basic JSON turn must produce a completed response
//! through the full wire stack (axum server, engine, in-memory store). The
//! remaining rig tests port from `serdes-ai-responses` in a follow-up.

mod rig;

use rig::{recording_model, spawn_server};
use serde_json::{Value, json};

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

#[tokio::test]
async fn json_turn_returns_completed_response() {
    let (model, calls) = recording_model();
    let addr = spawn_server(model).await;

    let response = client()
        .post(format!("http://{addr}/v1/responses"))
        .json(&json!({
            "model": "my-model",
            "instructions": "be brief",
            "input": "hello",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert!(body["id"].as_str().unwrap().starts_with("resp_"));
    assert_eq!(body["status"], "completed");
    assert_eq!(body["model"], "my-model");
    assert_eq!(body["output"][0]["content"][0]["text"], "ok");
    // instructions + user prompt form the history
    assert_eq!(*calls.lock().unwrap(), vec![2]);
}
