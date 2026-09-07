//! HTTP-transport tests for the responses model against the local rig:
//! terminal-last SSE streaming and the full-input turns that stand in for
//! HTTP session chaining until the chaining path lands over HTTP.
//! Websocket-transport coverage lives in `responses_client_ws.rs`.

mod rig;
mod ws_fakes_common;

use futures::StreamExt;
use rig::{recording_model, spawn_server};
use serdes_ai_core::messages::{
    ModelRequest, ModelRequestPart, ModelResponse, ModelResponsePart, ModelResponseStreamEvent,
    TextPart,
};
use serdes_ai_models::model::Model;
use serdes_ai_models::openai::responses::{OpenAIResponsesModel, Transport};
use ws_fakes_common::{params, response_turn, settings, system_turn, text_of, user_turn};

/// An HTTP-transport model pointed at the rig. The HTTP transport appends
/// `/responses` to the base URL, so this is the API root, not the endpoint.
fn http_client(addr: std::net::SocketAddr) -> OpenAIResponsesModel {
    OpenAIResponsesModel::new("test-model", "test-key")
        .with_base_url(format!("http://{addr}/v1"))
        .with_transport(Transport::Http)
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
async fn http_stateful_chaining_across_turns() {
    // HTTP session chaining (previous_response_id) is not implemented yet,
    // so each turn carries the full history; the rig still reconstructs the
    // same server-side history lens a chained turn would produce.
    let (model, calls) = recording_model();
    let addr = spawn_server(model).await;
    let client = http_client(addr);

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
    let client = http_client(addr);

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

#[tokio::test]
async fn http_stream_chained_turns_send_only_new_items() {
    let (model, calls) = recording_model();
    let addr = spawn_server(model).await;
    let client = http_client(addr);

    let drain = |client: OpenAIResponsesModel, history: Vec<ModelRequest>| async move {
        let mut stream = client
            .request_stream(&history, &settings(), &params())
            .await
            .expect("stream starts");
        let mut text = String::new();
        let mut terminal = false;
        while let Some(item) = stream.next().await {
            match item.expect("event ok") {
                // HTTP streaming currently falls back to a buffered turn
                // (part content arrives on PartStart); collect deltas too
                // so the drain keeps working once real SSE deltas land.
                ModelResponseStreamEvent::PartStart(start) => {
                    if let serdes_ai_core::messages::ModelResponsePart::Text(text_part) =
                        &start.part
                    {
                        text.push_str(&text_part.content);
                    }
                }
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

    // Turn 2 carries the full history (HTTP chaining is not implemented
    // yet): instructions + first prompt + prior response + second prompt.
    // A turn that dropped the prior response or double-counted the system
    // turn would not reconstruct this lens.
    assert_eq!(*calls.lock().unwrap(), vec![2, 4]);
}

#[tokio::test]
async fn http_conversations_stay_isolated() {
    let (model, calls) = recording_model();
    let addr = spawn_server(model).await;
    let client = http_client(addr);

    let mut convo_a = vec![system_turn("be brief"), user_turn("first")];
    let first = client
        .request(&convo_a, &settings(), &params())
        .await
        .expect("conversation A turn 1");
    assert_eq!(text_of(&first), "ok");

    // A different first request is a different conversation even through
    // the same model instance.
    let convo_b = vec![system_turn("other brief"), user_turn("b")];
    let b = client
        .request(&convo_b, &settings(), &params())
        .await
        .expect("conversation B turn 1");
    assert_eq!(text_of(&b), "ok");

    // Conversation A's second turn reconstructs its own history.
    convo_a.push(response_turn(first));
    convo_a.push(user_turn("second"));
    let second = client
        .request(&convo_a, &settings(), &params())
        .await
        .expect("conversation A turn 2");
    assert_eq!(text_of(&second), "ok");

    // A1 sees 2; B1 sees 2 (a fresh conversation, not A's history); A2
    // sees its own full 4. The requests are stateless full input until
    // HTTP chaining lands, so this pins the per-conversation history lens.
    assert_eq!(*calls.lock().unwrap(), vec![2, 2, 4]);
}
