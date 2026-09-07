//! Recovery-path coverage for the responses websocket transport.
//!
//! The scripted fake servers here observe the exact frames the client
//! sends: stale `previous_response_not_found` continuation replay,
//! reconnect after the server's connection lifetime limit, hard error
//! surfacing, and the no-replay guarantee once a stream event has escaped
//! to the caller. Full protocol behavior (mapping, delta-only chained
//! sends, conversation isolation) is covered against the local rig by
//! `responses_rig.rs` and `responses_client_ws.rs`, and by the session
//! unit tests.
#![cfg(feature = "responses-ws")]

use futures::{SinkExt, StreamExt};
use serdes_ai_core::messages::{ModelRequest, ModelRequestPart, ModelResponseStreamEvent};
use serdes_ai_models::ModelError;
use serdes_ai_models::model::Model;
use serdes_ai_models::openai::responses::events::StreamEvent;
use serdes_ai_models::openai::responses::wire::{OutputItem, OutputItemStatus, ResponseInput};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

mod ws_fakes_common;

use ws_fakes_common::{
    accept_ws, params, read_turn, send_completed_turn, send_event, settings, system_turn, text_of,
    user_turn, ws_client,
};

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
            ResponseInput::Items(items) => items.len(),
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
        ws.send(Message::text(envelope.to_string())).await.unwrap();

        // Retry: no continuation id, full input replayed.
        let replay = read_turn(&mut ws).await;
        assert!(replay.previous_response_id.is_none());
        let items = match &replay.input {
            ResponseInput::Items(items) => items.len(),
            other => panic!("expected items, got {other:?}"),
        };
        assert!(
            items >= 2,
            "replay carries the full input, got {items} items"
        );
        send_completed_turn(&mut ws, "resp_2", &replay).await;
    });

    let client = ws_client(addr).with_session_chaining(true);
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
        ws.send(Message::text(envelope.to_string())).await.unwrap();
        ws.send(Message::Close(None)).await.unwrap();
        drop(ws);

        // Second connection: fresh session, full input, no continuation.
        let mut ws = accept_ws(&listener).await;
        let request = read_turn(&mut ws).await;
        assert!(request.previous_response_id.is_none());
        send_completed_turn(&mut ws, "resp_1", &request).await;
    });

    let client = ws_client(addr);
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
        ws.send(Message::text(envelope.to_string())).await.unwrap();
    });

    let client = ws_client(addr);
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
                    status: OutputItemStatus::InProgress,
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
        ws.send(Message::text(envelope.to_string())).await.unwrap();

        // The delta already escaped to the caller, so the client must NOT
        // send another response.create frame on this turn.
        match tokio::time::timeout(Duration::from_millis(300), ws.next()).await {
            Err(_elapsed) => {}
            Ok(frame) => panic!("client replayed a committed stream: {frame:?}"),
        }
    });

    let client = ws_client(addr).with_session_chaining(true);
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
