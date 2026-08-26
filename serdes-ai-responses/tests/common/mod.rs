// Helpers are shared across test binaries; not every binary uses every one.
#![allow(dead_code)]

//! Shared helpers for the integration tests.

use serdes_ai_models::mock::FunctionModel;
use serdes_ai_responses::engine::ResponsesEngine;
use serdes_ai_responses::server::ResponsesServer;
use serdes_ai_responses::websocket::WebSocketSessionConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

/// A function model that records how many model requests each turn saw.
///
/// The non-streaming and streaming paths share the record, so tests can
/// assert on history lengths regardless of transport.
pub fn recording_model() -> (FunctionModel, Arc<std::sync::Mutex<Vec<usize>>>) {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls_stream = calls.clone();
    let calls_out = calls.clone();
    let model = FunctionModel::with_both(
        move |requests, _| {
            calls.lock().unwrap().push(requests.len());
            serdes_ai_core::ModelResponse::text("ok")
        },
        move |requests, _| {
            calls_stream.lock().unwrap().push(requests.len());
            let part = serdes_ai_core::messages::TextPart::new("ok");
            Box::pin(futures::stream::iter(vec![
                Ok(serdes_ai_core::ModelResponseStreamEvent::part_start(
                    0,
                    serdes_ai_core::ModelResponsePart::Text(part),
                )),
                Ok(serdes_ai_core::ModelResponseStreamEvent::StreamComplete(
                    serdes_ai_core::messages::StreamCompleteEvent {
                        finish_reason: serdes_ai_core::FinishReason::Stop,
                        input_tokens: Some(1),
                        output_tokens: Some(1),
                        cache_creation_tokens: None,
                        cache_read_tokens: None,
                    },
                )),
            ]))
        },
    );
    (model, calls_out)
}

/// Start the responses server on an ephemeral port and return its address.
pub async fn spawn_server(model: FunctionModel) -> SocketAddr {
    spawn_server_with_ws_config(model, WebSocketSessionConfig::default()).await
}

/// Start the responses server with a custom websocket session config.
pub async fn spawn_server_with_ws_config(
    model: FunctionModel,
    websocket_config: WebSocketSessionConfig,
) -> SocketAddr {
    let engine = ResponsesEngine::new(Arc::new(model));
    let server = ResponsesServer::new(engine).with_websocket_config(websocket_config);
    let router = server.router();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}
