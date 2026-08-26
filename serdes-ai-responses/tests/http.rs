//! HTTP integration tests for the test rig: JSON turns, SSE streaming,
//! stateful chaining, and error envelopes.

mod common;

use common::{recording_model, spawn_server};
use serde_json::{json, Value};

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

#[tokio::test]
async fn stateful_chaining_resolves_previous_response_id() {
    let (model, calls) = recording_model();
    let addr = spawn_server(model).await;
    let url = format!("http://{addr}/v1/responses");

    let first: Value = client()
        .post(&url)
        .json(&json!({"model": "m", "instructions": "sys", "input": "first"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let first_id = first["id"].as_str().unwrap().to_string();

    let second: Value = client()
        .post(&url)
        .json(&json!({
            "model": "m",
            "previous_response_id": first_id,
            "instructions": "new sys",
            "input": "second",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["status"], "completed");

    // Turn 1: instructions + prompt. Turn 2: replaced instructions + first
    // turn history + second prompt.
    let calls = calls.lock().unwrap();
    assert_eq!(*calls, vec![2, 4]);
}

#[tokio::test]
async fn sse_stream_frames_and_done_sentinel() {
    let (model, _calls) = recording_model();
    let addr = spawn_server(model).await;

    let response = client()
        .post(format!("http://{addr}/v1/responses"))
        .json(&json!({"model": "m", "input": "hello", "stream": true}))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = response.text().await.unwrap();
    assert!(body.starts_with("data: {"));
    assert!(body.contains("\"type\":\"response.created\""));
    assert!(body.contains("\"type\":\"response.completed\""));
    assert!(body.ends_with("data: [DONE]\n\n"));

    // Sequence numbers restart per event stream and are contiguous.
    let sequences: Vec<u64> = body
        .lines()
        .filter(|line| line.starts_with("data: {"))
        .filter_map(|line| serde_json::from_str::<Value>(&line[6..]).ok())
        .filter_map(|event| event["sequence_number"].as_u64())
        .collect();
    let expected: Vec<u64> = (0..sequences.len() as u64).collect();
    assert_eq!(sequences, expected);
}

#[tokio::test]
async fn malformed_body_returns_invalid_request_envelope() {
    let (model, _calls) = recording_model();
    let addr = spawn_server(model).await;

    let response = client()
        .post(format!("http://{addr}/v1/responses"))
        .header("content-type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_request_error");
}

#[tokio::test]
async fn unknown_previous_response_id_is_404_with_code() {
    let (model, _calls) = recording_model();
    let addr = spawn_server(model).await;

    let response = client()
        .post(format!("http://{addr}/v1/responses"))
        .json(&json!({"model": "m", "previous_response_id": "resp_missing", "input": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "previous_response_not_found");
}

#[tokio::test]
async fn background_mode_is_rejected() {
    let (model, _calls) = recording_model();
    let addr = spawn_server(model).await;

    let response = client()
        .post(format!("http://{addr}/v1/responses"))
        .json(&json!({"model": "m", "input": "x", "background": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_request_error");
}

#[tokio::test]
async fn builtin_tools_are_rejected() {
    let (model, _calls) = recording_model();
    let addr = spawn_server(model).await;

    let response = client()
        .post(format!("http://{addr}/v1/responses"))
        .json(&json!({
            "model": "m",
            "input": "x",
            "tools": [{"type": "web_search_preview"}],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["code"], "invalid_request_error");
}

#[tokio::test]
async fn health_check() {
    let (model, _calls) = recording_model();
    let addr = spawn_server(model).await;

    let response = client()
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "ok");
}
