//! Integration tests for [`OpenResponsesModel`], the client-side `Model`.
//!
//! End-to-end behavior (mapping, delta-only continuation sends, stateful
//! HTTP chaining, TTL reconnect) runs against the local test rig. Recovery
//! paths that the rig cannot be coaxed into (`previous_response_not_found`
//! replay, connection-limit reconnect) run against scripted fake servers so
//! the exact frames the client sends are observable.

mod common;

use common::{recording_model, spawn_server, spawn_server_with_ws_config};
use futures::{SinkExt, StreamExt};
use serdes_ai_core::messages::{
    ModelRequest, ModelRequestPart, ModelResponse, ModelResponsePart, ModelResponseStreamEvent,
    SystemPromptPart, TextPart, UserPromptPart,
};
use serdes_ai_models::model::{Model, ModelRequestParameters};
use serdes_ai_models::ModelError;
use serdes_ai_responses::client::OpenResponsesModel;
use serdes_ai_responses::types::{
    CreateResponseRequest, OutputContent, OutputItem, ResponseObject, ResponseStatus,
    ResponseUsage, StreamEvent,
};
use serdes_ai_tools::ToolDefinition;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{accept_async, WebSocketStream};

/// A user turn with a plain text prompt.
fn user_turn(text: &str) -> ModelRequest {
    ModelRequest::with_parts(vec![ModelRequestPart::UserPrompt(UserPromptPart::new(
        text,
    ))])
}

/// A system turn.
fn system_turn(text: &str) -> ModelRequest {
    ModelRequest::with_parts(vec![ModelRequestPart::SystemPrompt(SystemPromptPart::new(
        text,
    ))])
}

fn params() -> ModelRequestParameters {
    ModelRequestParameters::new()
}

fn settings() -> serdes_ai_core::ModelSettings {
    serdes_ai_core::ModelSettings::default()
}

/// Concatenated text parts of a response.
fn text_of(response: &serdes_ai_core::ModelResponse) -> String {
    response
        .text_parts()
        .map(|part| part.content.as_str())
        .collect()
}

// ---------------------------------------------------------------------------
// End-to-end against the rig: websocket transport
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ws_turns_map_responses_and_send_only_new_items() {
    let (model, calls) = recording_model();
    let addr = spawn_server(model).await;
    let client = OpenResponsesModel::new("test-model", format!("ws://{addr}/v1/responses"));

    let mut history = vec![system_turn("be brief"), user_turn("first")];
    let first = client
        .request(&history, &settings(), &params())
        .await
        .expect("first turn");

    assert_eq!(text_of(&first), "ok");
    assert!(first
        .vendor_id
        .as_deref()
        .unwrap_or("")
        .starts_with("resp_"));
    let first_id = first.vendor_id.clone().unwrap();

    history.push(ModelRequest::with_parts(vec![
        ModelRequestPart::ModelResponse(Box::new(first)),
    ]));
    history.push(user_turn("second"));
    let second = client
        .request(&history, &settings(), &params())
        .await
        .expect("second turn");

    // Turn 1 sees instructions + prompt. Turn 2 must see the chained
    // history reconstructed server-side (instructions + prompt + prior
    // response + prompt = 4); a client that replayed its full input every
    // turn would show 6 instead.
    assert_eq!(*calls.lock().unwrap(), vec![2, 4]);
    assert_eq!(text_of(&second), "ok");
    assert_ne!(second.vendor_id.as_deref(), Some(first_id.as_str()));
}

#[tokio::test]
async fn ws_stream_emits_events_with_terminal_last() {
    let (model, _calls) = recording_model();
    let addr = spawn_server(model).await;
    let client = OpenResponsesModel::new("test-model", format!("ws://{addr}/v1/responses"));

    let history = vec![user_turn("hello")];
    let mut stream = client
        .request_stream(&history, &settings(), &params())
        .await
        .expect("stream starts");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("event ok"));
    }

    let terminals = events
        .iter()
        .filter(|event| matches!(event, ModelResponseStreamEvent::StreamComplete(_)))
        .count();
    assert_eq!(terminals, 1, "exactly one terminal event");
    assert!(matches!(
        events.last(),
        Some(ModelResponseStreamEvent::StreamComplete(_))
    ));
    assert!(matches!(
        events.first(),
        Some(ModelResponseStreamEvent::PartStart(_))
    ));
}

#[tokio::test]
async fn ws_connection_limit_reconnects_and_replays() {
    let (model, calls) = recording_model();
    let config = serdes_ai_responses::websocket::WebSocketSessionConfig {
        connection_ttl: Duration::ZERO,
    };
    let addr = spawn_server_with_ws_config(model, config).await;
    let client = OpenResponsesModel::new("test-model", format!("ws://{addr}/v1/responses"));

    let mut history = vec![system_turn("sys"), user_turn("first")];
    let first = client
        .request(&history, &settings(), &params())
        .await
        .expect("first turn");
    history.push(ModelRequest::with_parts(vec![
        ModelRequestPart::ModelResponse(Box::new(first)),
    ]));
    history.push(user_turn("second"));

    // The rig refuses the second turn on the aged socket
    // (websocket_connection_limit_reached); the client must reconnect,
    // start a fresh session, and replay the full input.
    let second = client
        .request(&history, &settings(), &params())
        .await
        .expect("second turn after reconnect");
    assert_eq!(text_of(&second), "ok");
    assert_eq!(*calls.lock().unwrap(), vec![2, 4]);
}

// ---------------------------------------------------------------------------
// End-to-end against the rig: HTTP transport
// ---------------------------------------------------------------------------

#[tokio::test]
async fn http_stateful_chaining_across_turns() {
    let (model, calls) = recording_model();
    let addr = spawn_server(model).await;
    let client = OpenResponsesModel::new("test-model", format!("http://{addr}/v1/responses"));

    let mut history = vec![system_turn("be brief"), user_turn("first")];
    let first = client
        .request(&history, &settings(), &params())
        .await
        .expect("first turn");
    assert_eq!(text_of(&first), "ok");

    history.push(ModelRequest::with_parts(vec![
        ModelRequestPart::ModelResponse(Box::new(first)),
    ]));
    history.push(user_turn("second"));
    let second = client
        .request(&history, &settings(), &params())
        .await
        .expect("second turn");

    assert_eq!(text_of(&second), "ok");
    assert_eq!(*calls.lock().unwrap(), vec![2, 4]);
}

#[tokio::test]
async fn http_stream_yields_events_with_terminal_last() {
    let (model, _calls) = recording_model();
    let addr = spawn_server(model).await;
    let client = OpenResponsesModel::new("test-model", format!("http://{addr}/v1/responses"));

    let history = vec![user_turn("hello")];
    let mut stream = client
        .request_stream(&history, &settings(), &params())
        .await
        .expect("stream starts");

    let mut saw_terminal = false;
    let mut count = 0;
    while let Some(event) = stream.next().await {
        let event = event.expect("event ok");
        count += 1;
        match &event {
            ModelResponseStreamEvent::StreamComplete(_) => saw_terminal = true,
            _ => assert!(!saw_terminal, "terminal event must be last"),
        }
    }
    assert!(saw_terminal);
    assert!(count >= 2);
}

// ---------------------------------------------------------------------------
// Recovery paths against scripted fake servers
// ---------------------------------------------------------------------------

type FakeWs = WebSocketStream<tokio::net::TcpStream>;

/// Accept one websocket connection.
async fn accept_ws(listener: &TcpListener) -> FakeWs {
    let (stream, _) = listener.accept().await.unwrap();
    accept_async(stream).await.unwrap()
}

/// Read one `response.create` frame from the client.
async fn read_turn(ws: &mut FakeWs) -> CreateResponseRequest {
    loop {
        let message = ws.next().await.expect("frame").expect("ws ok");
        match message {
            Message::Text(text) => {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(value["type"], "response.create", "unexpected frame: {text}");
                return serde_json::from_value(value["response"].clone()).unwrap();
            }
            Message::Close(_) => panic!("client closed before sending a turn"),
            _ => continue,
        }
    }
}

/// Run a minimal-but-realistic turn: item added, text delta, item done,
/// completed. The client assembles parts from the streamed item events, so
/// a bare `response.completed` would leave the folded response empty.
async fn send_completed_turn(ws: &mut FakeWs, id: &str, request: &CreateResponseRequest) {
    let mut response = ResponseObject::in_progress(id, 0, request.model.clone(), request);
    response.status = ResponseStatus::Completed;
    response.output = vec![OutputItem::Message {
        id: format!("msg_{id}"),
        role: "assistant".to_string(),
        status: serdes_ai_responses::types::OutputItemStatus::Completed,
        content: vec![OutputContent::OutputText {
            text: "ok".to_string(),
            annotations: Vec::new(),
        }],
    }];
    response.usage = Some(ResponseUsage {
        input_tokens: Some(1),
        output_tokens: Some(1),
        total_tokens: Some(2),
    });

    let events = vec![
        StreamEvent::OutputItemAdded {
            sequence_number: 1,
            output_index: 0,
            item: OutputItem::Message {
                id: format!("msg_{id}"),
                role: "assistant".to_string(),
                status: serdes_ai_responses::types::OutputItemStatus::InProgress,
                content: Vec::new(),
            },
        },
        StreamEvent::OutputTextDelta {
            sequence_number: 2,
            item_id: format!("msg_{id}"),
            output_index: 0,
            content_index: 0,
            delta: "ok".to_string(),
        },
        StreamEvent::OutputItemDone {
            sequence_number: 3,
            output_index: 0,
            item: response.output[0].clone(),
        },
        StreamEvent::ResponseCompleted {
            sequence_number: 4,
            response,
        },
    ];
    for event in &events {
        send_event(ws, event).await;
    }
}

async fn send_event(ws: &mut FakeWs, event: &StreamEvent) {
    ws.send(Message::Text(serde_json::to_string(event).unwrap()))
        .await
        .unwrap();
}

#[tokio::test]
async fn stale_continuation_clears_chain_and_replays_full_input() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_ws(&listener).await;

        // Turn 1: full input, no continuation.
        let request = read_turn(&mut ws).await;
        assert!(request.previous_response_id.is_none());
        send_completed_turn(&mut ws, "resp_1", &request).await;

        // Turn 2: client chains onto resp_1 and sends only the new item.
        let request = read_turn(&mut ws).await;
        assert_eq!(request.previous_response_id.as_deref(), Some("resp_1"));
        let items = match &request.input {
            serdes_ai_responses::types::ResponseInput::Items(items) => items.len(),
            other => panic!("expected items, got {other:?}"),
        };
        assert_eq!(items, 1, "chained turn sends only new input");

        // The chain is stale from the server's point of view.
        let envelope = serde_json::json!({
            "type": "error",
            "status_code": 404,
            "error": {
                "code": "previous_response_not_found",
                "message": "previous response not found: resp_1",
            }
        });
        ws.send(Message::Text(envelope.to_string())).await.unwrap();

        // Retry: no continuation id, full input replayed.
        let replay = read_turn(&mut ws).await;
        assert!(replay.previous_response_id.is_none());
        let items = match &replay.input {
            serdes_ai_responses::types::ResponseInput::Items(items) => items.len(),
            other => panic!("expected items, got {other:?}"),
        };
        assert!(
            items >= 2,
            "replay carries the full input, got {items} items"
        );
        send_completed_turn(&mut ws, "resp_2", &replay).await;
    });

    let client = OpenResponsesModel::new("test-model", format!("ws://{addr}/v1/responses"));
    let mut history = vec![system_turn("sys"), user_turn("first")];
    let first = client
        .request(&history, &settings(), &params())
        .await
        .expect("first turn");
    history.push(ModelRequest::with_parts(vec![
        ModelRequestPart::ModelResponse(Box::new(first)),
    ]));
    history.push(user_turn("second"));

    let second = client
        .request(&history, &settings(), &params())
        .await
        .expect("second turn after stale-continuation recovery");
    assert_eq!(text_of(&second), "ok");
    server.await.unwrap();
}

#[tokio::test]
async fn connection_limit_error_reconnects_on_a_fresh_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        // First connection: refuse the very first turn with the limit error
        // and drop the socket.
        let mut ws = accept_ws(&listener).await;
        let _request = read_turn(&mut ws).await;
        let envelope = serde_json::json!({
            "type": "error",
            "status_code": 429,
            "error": {
                "code": "websocket_connection_limit_reached",
                "message": "websocket connection lifetime limit reached",
            }
        });
        ws.send(Message::Text(envelope.to_string())).await.unwrap();
        ws.send(Message::Close(None)).await.unwrap();
        drop(ws);

        // Second connection: fresh session, full input, no continuation.
        let mut ws = accept_ws(&listener).await;
        let request = read_turn(&mut ws).await;
        assert!(request.previous_response_id.is_none());
        send_completed_turn(&mut ws, "resp_1", &request).await;
    });

    let client = OpenResponsesModel::new("test-model", format!("ws://{addr}/v1/responses"));
    let response = client
        .request(&[user_turn("hello")], &settings(), &params())
        .await
        .expect("turn succeeds after reconnect");
    assert_eq!(text_of(&response), "ok");
    server.await.unwrap();
}

#[tokio::test]
async fn hard_error_surfaces_as_model_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_ws(&listener).await;
        let _request = read_turn(&mut ws).await;
        let envelope = serde_json::json!({
            "type": "error",
            "status_code": 502,
            "error": {"code": "model_error", "message": "model boom"}
        });
        ws.send(Message::Text(envelope.to_string())).await.unwrap();
    });

    let client = OpenResponsesModel::new("test-model", format!("ws://{addr}/v1/responses"));
    let error = client
        .request(&[user_turn("hello")], &settings(), &params())
        .await
        .expect_err("turn must fail");
    match &error {
        ModelError::Provider { code, message, .. } => {
            assert_eq!(code, "model_error");
            assert!(message.contains("boom"));
        }
        other => panic!("expected provider error, got {other:?}"),
    }
    server.await.unwrap();
}

/// A minimal assistant response for extending history in tests.
fn text_response(text: &str) -> ModelResponse {
    ModelResponse {
        parts: vec![ModelResponsePart::Text(TextPart::new(text))],
        model_name: None,
        timestamp: chrono::Utc::now(),
        finish_reason: None,
        usage: None,
        vendor_id: None,
        vendor_details: None,
        kind: "response".to_string(),
    }
}

#[tokio::test]
async fn mid_stream_error_is_surfaced_without_replay() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_ws(&listener).await;

        // Turn 1 completes normally so the client chains onto resp_1.
        let request = read_turn(&mut ws).await;
        assert!(request.previous_response_id.is_none());
        send_completed_turn(&mut ws, "resp_1", &request).await;

        // Turn 2 starts streaming, then fails mid-stream with a code the
        // client would normally treat as recoverable.
        let request = read_turn(&mut ws).await;
        assert_eq!(request.previous_response_id.as_deref(), Some("resp_1"));
        send_event(
            &mut ws,
            &StreamEvent::OutputItemAdded {
                sequence_number: 1,
                output_index: 0,
                item: OutputItem::Message {
                    id: "msg_partial".to_string(),
                    role: "assistant".to_string(),
                    status: serdes_ai_responses::types::OutputItemStatus::InProgress,
                    content: Vec::new(),
                },
            },
        )
        .await;
        send_event(
            &mut ws,
            &StreamEvent::OutputTextDelta {
                sequence_number: 2,
                item_id: "msg_partial".to_string(),
                output_index: 0,
                content_index: 0,
                delta: "par".to_string(),
            },
        )
        .await;
        let envelope = serde_json::json!({
            "type": "error",
            "status_code": 404,
            "error": {
                "code": "previous_response_not_found",
                "message": "previous response not found: resp_1",
            }
        });
        ws.send(Message::Text(envelope.to_string())).await.unwrap();

        // The delta already escaped to the caller, so the client must NOT
        // send another response.create frame on this turn.
        match tokio::time::timeout(Duration::from_millis(300), ws.next()).await {
            Err(_elapsed) => {}
            Ok(frame) => panic!("client replayed a committed stream: {frame:?}"),
        }
    });

    let client = OpenResponsesModel::new("test-model", format!("ws://{addr}/v1/responses"));
    let history = vec![user_turn("hello")];
    let first = client
        .request(&history, &settings(), &params())
        .await
        .expect("first turn");

    let mut history = history;
    history.push(ModelRequest::with_parts(vec![
        ModelRequestPart::ModelResponse(Box::new(first)),
    ]));
    history.push(user_turn("second"));
    let mut stream = client
        .request_stream(&history, &settings(), &params())
        .await
        .expect("stream starts");

    let mut saw_delta = false;
    let mut error = None;
    while let Some(item) = stream.next().await {
        match item {
            Ok(ModelResponseStreamEvent::PartDelta(_)) => saw_delta = true,
            Ok(ModelResponseStreamEvent::StreamComplete(_)) => {
                panic!("mid-stream failure must not produce a terminal event")
            }
            Ok(_) => {}
            Err(err) => error = Some(err),
        }
    }
    assert!(saw_delta, "streamed delta must reach the caller");
    match error.expect("mid-stream failure must surface as an error item") {
        ModelError::Provider { code, .. } => assert_eq!(code, "previous_response_not_found"),
        other => panic!("expected provider error, got {other:?}"),
    }
    server.await.unwrap();
}

#[tokio::test]
async fn http_stream_chained_turns_send_only_new_items() {
    let (model, calls) = recording_model();
    let addr = spawn_server(model).await;
    let client = OpenResponsesModel::new("test-model", format!("http://{addr}/v1/responses"));

    let drain = |client: OpenResponsesModel, history: Vec<ModelRequest>| async move {
        let mut stream = client
            .request_stream(&history, &settings(), &params())
            .await
            .expect("stream starts");
        let mut text = String::new();
        let mut terminal = false;
        while let Some(item) = stream.next().await {
            match item.expect("event ok") {
                ModelResponseStreamEvent::PartDelta(delta) => {
                    if let serdes_ai_core::messages::ModelResponsePartDelta::Text(text_delta) =
                        delta.delta
                    {
                        text.push_str(&text_delta.content_delta);
                    }
                }
                ModelResponseStreamEvent::StreamComplete(_) => terminal = true,
                _ => {}
            }
        }
        assert!(terminal, "stream must end with a terminal event");
        text
    };

    let mut history = vec![system_turn("be brief"), user_turn("first")];
    let first_text = drain(client.clone(), history.clone()).await;
    assert_eq!(first_text, "ok");

    history.push(ModelRequest::with_parts(vec![
        ModelRequestPart::ModelResponse(Box::new(text_response(&first_text))),
    ]));
    history.push(user_turn("second"));
    let second_text = drain(client, history).await;
    assert_eq!(second_text, "ok");

    // Turn 2 must send only the new user item; a client that replayed the
    // prior ModelResponse would show 5 instead of 4.
    assert_eq!(*calls.lock().unwrap(), vec![2, 4]);
}

#[tokio::test]
async fn client_sends_function_tools_with_wire_type_tag() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let mut ws = accept_ws(&listener).await;
        // Capture the raw frame: the type tag must be present on the wire,
        // not just in the parsed form.
        let value = loop {
            match ws.next().await.unwrap().unwrap() {
                Message::Text(text) => {
                    break serde_json::from_str::<serde_json::Value>(&text).unwrap()
                }
                Message::Close(_) => panic!("client closed before sending a turn"),
                _ => continue,
            }
        };
        assert_eq!(value["type"], "response.create");
        let tools = value["response"]["tools"]
            .as_array()
            .expect("tools on the wire");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["name"], "get_weather");
        assert_eq!(tools[0]["strict"], true);
        assert_eq!(tools[0]["parameters"]["type"], "object");

        let request =
            serde_json::from_value::<CreateResponseRequest>(value["response"].clone()).unwrap();
        send_completed_turn(&mut ws, "resp_1", &request).await;
    });

    let client = OpenResponsesModel::new("test-model", format!("ws://{addr}/v1/responses"));
    let tool = ToolDefinition {
        name: "get_weather".to_string(),
        description: "Look up weather".to_string(),
        parameters_json_schema: serde_json::json!({
            "type": "object",
            "properties": {"city": {"type": "string"}}
        }),
        strict: Some(true),
        outer_typed_dict_key: None,
    };
    let params = ModelRequestParameters::new().with_tools(vec![tool]);
    let response = client
        .request(&[user_turn("weather in NYC?")], &settings(), &params)
        .await
        .expect("turn with tools");
    assert_eq!(text_of(&response), "ok");
    server.await.unwrap();
}
