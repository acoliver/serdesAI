//! Open Responses websocket transport.
//!
//! `GET /v1/responses` upgrades to a websocket. The client sends
//! `{"type":"response.create","response":{...}}` text frames; the server
//! answers with the same event objects the SSE transport emits, one JSON
//! object per text frame, with `sequence_number` restarting at 0 each turn.
//!
//! Semantics (Open Responses websocket profile):
//!
//! - Turns are sequential: the server processes one `response.create` at a
//!   time.
//! - `stream`, `stream_options`, and `background` must be omitted; events
//!   always stream over the socket and background mode is unsupported.
//! - `store: false` turns keep their continuation state in a connection-local
//!   cache, so nothing is persisted globally while still allowing
//!   `previous_response_id` chaining on the same connection (the codex CLI
//!   default profile).
//! - When a continuation references an evicted or unknown response ID, the
//!   server replies with a `previous_response_not_found` envelope; the
//!   referenced ID is evicted from the session cache so the client is pushed
//!   to replay the full input, matching how the codex CLI recovers.
//! - The connection has a lifetime limit (default 60 minutes, enforced
//!   between turns only); exceeding it closes the socket with a
//!   `websocket_connection_limit_reached` envelope, which clients treat as a
//!   reconnect signal.
//! - Errors are wrapped as `{"type":"error","status_code":N,"error":{...}}`.
//! - The end of a turn is signaled by its terminal event
//!   (`response.completed`, `response.incomplete`, or `response.failed`),
//!   not by a channel close; turns are sequential, so the client always knows
//!   which turn an event belongs to.

use crate::engine::ResponsesEngine;
use crate::error::{codes, ResponsesError, WsErrorEnvelope};
use crate::store::SessionResponseCache;
use crate::types::StreamEvent;
use axum::extract::ws::{Message, WebSocket};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Configuration for websocket sessions.
#[derive(Debug, Clone)]
pub struct WebSocketSessionConfig {
    /// Maximum connection lifetime before the server refuses new turns.
    ///
    /// Enforced only between turns, never mid-turn.
    pub connection_ttl: Duration,
}

impl Default for WebSocketSessionConfig {
    fn default() -> Self {
        Self {
            connection_ttl: Duration::from_secs(60 * 60),
        }
    }
}

/// Keys that must not appear in a websocket `response.create` payload.
const FORBIDDEN_KEYS: [&str; 3] = ["stream", "stream_options", "background"];

/// Outgoing frame sender shared by turn execution and control paths.
type FrameSender = mpsc::UnboundedSender<Message>;

/// Serve one websocket connection until the client closes or the lifetime
/// limit is reached.
pub async fn handle_socket(
    socket: WebSocket,
    engine: Arc<ResponsesEngine>,
    config: WebSocketSessionConfig,
) {
    // Split the socket so a forwarding task can write frames while the
    // receive loop keeps reading; turns push events through an unbounded
    // channel, keeping the sync engine sink non-blocking.
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (msg_tx, mut msg_rx) = mpsc::unbounded::<Message>();
    let forwarder = tokio::spawn(async move {
        while let Some(message) = msg_rx.next().await {
            if ws_tx.send(message).await.is_err() {
                break;
            }
        }
        // Send a close frame for a clean shutdown when the client has not
        // already closed.
        let _ = ws_tx.close().await;
    });

    let session = SessionResponseCache::default();
    let connected_at = Instant::now();
    // The lifetime limit is enforced between turns only: a connection always
    // gets to serve its first turn, matching the Open Responses reconnect
    // contract (a fresh connection is a fresh allowance).
    let mut turns_served = false;

    while let Some(frame) = ws_rx.next().await {
        let frame = match frame {
            Ok(frame) => frame,
            Err(err) => {
                tracing::debug!("websocket receive error, closing: {err}");
                break;
            }
        };
        match frame {
            Message::Text(text) => {
                let mut sender = msg_tx.clone();
                let rejection = run_turn(
                    engine.as_ref(),
                    &session,
                    connected_at,
                    turns_served,
                    &config,
                    text.as_str(),
                    &mut sender,
                )
                .await;
                if let Some(error) = rejection {
                    // A rejected turn emitted no events, so the error
                    // envelope is the only signal the client gets. Evict the
                    // referenced continuation ID so a retried chain with the
                    // same ID fails fast and the client replays full input.
                    if let Some(id) = error.previous_response_id() {
                        session.evict(&id);
                    }
                    let envelope = WsErrorEnvelope::from_error(&error);
                    let _ =
                        sender.unbounded_send(Message::Text(envelope.to_json().to_string().into()));
                    if envelope.error.code == codes::WEBSOCKET_CONNECTION_LIMIT_REACHED {
                        let _ = sender.unbounded_send(Message::Close(None));
                        break;
                    }
                } else {
                    turns_served = true;
                }
            }
            Message::Ping(payload) => {
                let _ = msg_tx.unbounded_send(Message::Pong(payload));
            }
            Message::Pong(_) => {}
            Message::Close(_) => break,
            Message::Binary(_) => {
                let envelope = WsErrorEnvelope::from_error(&ResponsesError::InvalidRequest(
                    "binary frames are not supported; send response.create as a text frame"
                        .to_string(),
                ));
                let _ = msg_tx.unbounded_send(Message::Text(envelope.to_json().to_string().into()));
            }
        }
    }
    drop(msg_tx);
    let _ = forwarder.await;
}

/// Validate and run a single `response.create` frame.
///
/// Returns `Some(error)` when the turn was rejected before any event was
/// emitted (validation, unknown continuation, expired connection). Errors
/// during execution are streamed as a `response.failed` event, and the turn
/// still resolves as `None` because the connection remains usable.
async fn run_turn(
    engine: &ResponsesEngine,
    session: &SessionResponseCache,
    connected_at: Instant,
    turns_served: bool,
    config: &WebSocketSessionConfig,
    text: &str,
    sender: &mut FrameSender,
) -> Option<ResponsesError> {
    let frame: Value = match serde_json::from_str(text) {
        Ok(frame) => frame,
        Err(err) => {
            return Some(ResponsesError::InvalidRequest(format!(
                "frame is not valid JSON: {err}"
            )))
        }
    };
    let kind = match frame.get("type").and_then(Value::as_str) {
        Some(kind) => kind,
        None => {
            return Some(ResponsesError::InvalidRequest(
                "frame must carry a \"type\" field".to_string(),
            ))
        }
    };
    if kind != "response.create" {
        return Some(ResponsesError::InvalidRequest(format!(
            "unsupported frame type '{kind}'; only response.create is accepted"
        )));
    }
    let response = match frame.get("response") {
        Some(response) => response.clone(),
        None => {
            return Some(ResponsesError::InvalidRequest(
                "response.create frame must carry a response object".to_string(),
            ))
        }
    };
    let Some(response_object) = response.as_object() else {
        return Some(ResponsesError::InvalidRequest(
            "\"response\" must be an object".to_string(),
        ));
    };
    for key in FORBIDDEN_KEYS {
        if response_object.contains_key(key) {
            return Some(ResponsesError::InvalidRequest(format!(
                "'{key}' must be omitted in websocket turns"
            )));
        }
    }

    // The lifetime limit is enforced between turns only, so an in-flight turn
    // always completes.
    if turns_served && connected_at.elapsed() >= config.connection_ttl {
        return Some(ResponsesError::ConnectionLimitReached);
    }

    let request: crate::types::CreateResponseRequest = match serde_json::from_value(response) {
        Ok(request) => request,
        Err(err) => {
            return Some(ResponsesError::InvalidRequest(format!(
                "invalid response payload: {err}"
            )))
        }
    };

    let turn = match engine.prepare(&request, Some(session)).await {
        Ok(turn) => turn,
        Err(error) => return Some(error),
    };

    let mut streamed_anything = false;
    let mut sink = |event: StreamEvent| {
        streamed_anything = true;
        match serde_json::to_string(&event) {
            Ok(payload) => {
                let _ = sender.unbounded_send(Message::Text(payload.into()));
            }
            Err(err) => tracing::error!("failed to serialize stream event: {err}"),
        }
    };

    match engine.execute_streaming(turn, &mut sink).await {
        Ok(output) => {
            engine.persist(&request, Some(session), &output).await;
            None
        }
        Err(error) => {
            // A mid-stream failure already produced response.failed; only
            // acquisition failures (nothing streamed) need an envelope.
            if streamed_anything {
                tracing::warn!("websocket turn failed mid-stream: {error}");
                None
            } else {
                Some(error)
            }
        }
    }
}
