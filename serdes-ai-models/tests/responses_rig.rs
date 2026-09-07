//! Raw-protocol tests against the wire-accurate Open Responses rig adopted
//! under `tests/rig`: JSON and SSE turns over HTTP plus websocket turns
//! (sequential turns, session-local `store: false` state, continuation
//! errors, forbidden keys, ping/pong, connection lifetime), ported from the
//! `serdes-ai-responses` rig suite.

mod rig;

use futures::{SinkExt, StreamExt};
use rig::{recording_model, spawn_server, spawn_server_with_ws_config};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

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

// Websocket integration tests for the Open Responses transport: sequential
// turns, connection-local state for `store: false`, continuation recovery,
// forbidden keys, ping/pong, and the connection lifetime limit.

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(addr: &std::net::SocketAddr) -> Ws {
    let url = format!("ws://{addr}/v1/responses");
    tokio_tungstenite::connect_async(url)
        .await
        .expect("websocket upgrade on GET /v1/responses")
        .0
}

/// Send a response.create frame and collect events until the turn's terminal
/// event (or an error envelope) arrives.
async fn run_turn(ws: &mut Ws, response: Value) -> Vec<Value> {
    // Codex sends response.create frames flat: parameters on the frame root.
    let mut frame = response;
    frame["type"] = json!("response.create");
    ws.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();

    let mut events = Vec::new();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for event")
            .expect("socket closed mid-turn")
            .expect("socket error mid-turn");
        let Message::Text(text) = message else {
            continue;
        };
        let event: Value = serde_json::from_str(&text).unwrap();
        let done = matches!(
            event["type"].as_str(),
            Some("response.completed" | "response.incomplete" | "response.failed" | "error")
        );
        events.push(event);
        if done {
            return events;
        }
    }
}

fn find<'a>(events: &'a [Value], kind: &str) -> &'a Value {
    events
        .iter()
        .find(|event| event["type"] == kind)
        .unwrap_or_else(|| panic!("no {kind} event in {events:?}"))
}

#[tokio::test]
async fn turns_stream_events_with_terminal_completion() {
    let (model, _calls) = recording_model();
    let addr = spawn_server_with_ws_config(model, Default::default()).await;
    let mut ws = connect(&addr).await;

    let events = run_turn(
        &mut ws,
        json!({"model": "m", "instructions": "sys", "input": "hi"}),
    )
    .await;

    assert_eq!(find(&events, "response.created")["sequence_number"], 0);
    let completed = find(&events, "response.completed");
    assert_eq!(completed["response"]["status"], "completed");
    assert!(
        completed["response"]["id"]
            .as_str()
            .unwrap()
            .starts_with("resp_")
    );
    // wire names use the unprefixed forms for item-level events
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "response.output_item.added")
    );
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "response.output_text.delta")
    );

    // A second turn on the same socket works (sequential turns).
    let events = run_turn(&mut ws, json!({"model": "m", "input": "again"})).await;
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "response.completed")
    );
}

#[tokio::test]
async fn store_false_state_lives_in_the_session() {
    let (model, calls) = recording_model();
    let addr = spawn_server_with_ws_config(model, Default::default()).await;
    let mut ws = connect(&addr).await;

    // codex profile: store:false chaining on a single connection
    let events = run_turn(
        &mut ws,
        json!({"model": "m", "instructions": "sys", "input": "first", "store": false}),
    )
    .await;
    let first_id = find(&events, "response.completed")["response"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let events = run_turn(
        &mut ws,
        json!({
            "model": "m",
            "previous_response_id": first_id,
            "input": "second",
            "store": false,
        }),
    )
    .await;
    assert_eq!(
        find(&events, "response.completed")["response"]["status"],
        "completed"
    );

    // Turn 2 saw the full chained history: instructions + first prompt +
    // first model response + second prompt.
    let calls = calls.lock().unwrap();
    assert_eq!(*calls, vec![2, 4]);
}

#[tokio::test]
async fn unknown_continuation_reports_error_and_connection_survives() {
    let (model, _calls) = recording_model();
    let addr = spawn_server_with_ws_config(model, Default::default()).await;
    let mut ws = connect(&addr).await;

    let events = run_turn(
        &mut ws,
        json!({"model": "m", "previous_response_id": "resp_missing", "input": "x"}),
    )
    .await;
    let error = find(&events, "error");
    assert_eq!(error["error"]["code"], "previous_response_not_found");
    assert_eq!(error["status_code"], 404);

    // Full-input replay still works on the same connection.
    let events = run_turn(&mut ws, json!({"model": "m", "input": "replayed"})).await;
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "response.completed")
    );
}

#[tokio::test]
async fn stream_keys_are_ignored() {
    let (model, _calls) = recording_model();
    let addr = spawn_server_with_ws_config(model, Default::default()).await;
    let mut ws = connect(&addr).await;

    // The live backend accepts HTTP-only keys on websocket turns; the rig
    // strips them instead of rejecting the frame.
    let events = run_turn(&mut ws, json!({"model": "m", "input": "x", "stream": true})).await;
    assert!(
        find(&events, "response.completed").is_object(),
        "turn should complete with stream key present"
    );

    // Non response.create frames are rejected too.
    let events = run_turn_raw(&mut ws, json!({"type": "response.cancel"})).await;
    assert_eq!(
        find(&events, "error")["error"]["code"],
        "invalid_request_error"
    );
}

/// Like [`run_turn`] but sends an arbitrary frame.
async fn run_turn_raw(ws: &mut Ws, frame: Value) -> Vec<Value> {
    ws.send(Message::Text(frame.to_string().into()))
        .await
        .unwrap();
    let mut events = Vec::new();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for event")
            .expect("socket closed")
            .expect("socket error");
        let Message::Text(text) = message else {
            continue;
        };
        let event: Value = serde_json::from_str(&text).unwrap();
        let done = event["type"] == "error";
        events.push(event);
        if done {
            return events;
        }
    }
}

#[tokio::test]
async fn ping_is_answered_with_pong() {
    let (model, _calls) = recording_model();
    let addr = spawn_server_with_ws_config(model, Default::default()).await;
    let mut ws = connect(&addr).await;

    ws.send(Message::Ping(vec![1, 2, 3].into())).await.unwrap();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for pong")
            .expect("socket closed")
            .expect("socket error");
        match message {
            Message::Pong(payload) => {
                assert_eq!(payload, vec![1, 2, 3]);
                return;
            }
            Message::Text(_) => continue,
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

#[tokio::test]
async fn connection_lifetime_limit_closes_the_socket() {
    let (model, _calls) = recording_model();
    let config = rig::websocket::WebSocketSessionConfig {
        connection_ttl: Duration::from_millis(50),
    };
    let addr = spawn_server_with_ws_config(model, config).await;
    let mut ws = connect(&addr).await;

    // Before the TTL the turn works.
    let events = run_turn(&mut ws, json!({"model": "m", "input": "early"})).await;
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "response.completed")
    );

    tokio::time::sleep(Duration::from_millis(60)).await;

    // After the TTL the turn is refused, the envelope carries the codex
    // retryable code, and the server closes the socket.
    ws.send(Message::Text(
        json!({"type": "response.create", "response": {"model": "m", "input": "late"}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();

    let mut saw_limit_error = false;
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for close");
        let Some(message) = message else {
            assert!(saw_limit_error, "socket closed without the error envelope");
            return;
        };
        let message = message.expect("socket error");
        match message {
            Message::Text(text) => {
                let event: Value = serde_json::from_str(&text).unwrap();
                if event["type"] == "error" {
                    assert_eq!(event["error"]["code"], "websocket_connection_limit_reached");
                    assert_eq!(event["status_code"], 429);
                    saw_limit_error = true;
                }
            }
            Message::Close(_) => {
                assert!(saw_limit_error);
                return;
            }
            _ => {}
        }
    }
}

// HTTP integration tests for the test rig: SSE streaming, stateful
// chaining, and error envelopes. (The basic JSON turn lives at the top of
// this file.)

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
