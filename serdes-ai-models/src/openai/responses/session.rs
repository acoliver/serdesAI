//! Conversation-keyed session state for the responses model.
//!
//! Conversations are keyed by the fingerprint of the history's first
//! request, so different conversations through one model instance stay
//! isolated and run concurrently; each turn only sends the new input items
//! of its own conversation. The websocket transport consumes this state
//! today; the HTTP chaining path shares it in a later stage.
// With the websocket feature compiled out nothing reaches these items yet;
// that is expected, not dead code.
#![cfg_attr(not(feature = "responses-ws"), allow(dead_code))]

use crate::error::ModelError;
use async_trait::async_trait;
use serdes_ai_core::messages::{ModelRequest, ModelRequestPart, ModelResponseStreamEvent};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

/// Transport used to reach the endpoint.
///
/// HTTP is the default so the unified model's behavior is unchanged unless
/// the websocket transport is selected explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// HTTP (`https://`/`http://`): session chaining via `store: true` and
    /// `previous_response_id` on each `POST`.
    #[default]
    Http,
    /// Websocket (`wss://`/`ws://`): session-stateful turns with
    /// `store: false` and delta-only input. This is the transport the codex
    /// CLI and Open Responses servers are designed around.
    WebSocket,
}

impl Transport {
    /// Infer the transport from a URL scheme.
    #[must_use]
    pub fn from_url(url: &str) -> Self {
        if url.starts_with("http://") || url.starts_with("https://") {
            Self::Http
        } else {
            Self::WebSocket
        }
    }
}

/// How many times a turn may be retried internally (continuation replay
/// after `previous_response_not_found`, reconnect after a connection
/// failure or the server's connection lifetime limit) before the error
/// surfaces.
pub(crate) const MAX_ATTEMPTS: usize = 3;

/// Connection-local state for one conversation.
///
/// Conversations are keyed by the fingerprint of the history's first
/// request (see [`fingerprint`]). The websocket variant keeps the socket
/// alive across the conversation's turns; continuation state
/// (`previous_response_id`, the requests already sent) lives here, which is
/// what makes delta-only continuation turns possible. Turns of one
/// conversation are sequential (the protocol has no way to match
/// interleaved responses) while different conversations run concurrently,
/// each on its own socket.
pub(crate) struct Conv {
    /// Live socket for this conversation, connected lazily on the first
    /// websocket turn.
    #[cfg(feature = "responses-ws")]
    pub(crate) socket: Option<serdes_ai_streaming::websocket::WebSocketStream>,
    pub(crate) previous_response_id: Option<String>,
    /// Fingerprints of the requests the server already holds, in order. A
    /// turn's history must extend this sequence exactly; anything else
    /// voids the chain and forces a full replay.
    pub(crate) sent_fingerprints: Vec<u64>,
}

impl std::fmt::Debug for Conv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut state = f.debug_struct("Conv");
        #[cfg(feature = "responses-ws")]
        state.field("connected", &self.socket.is_some());
        state
            .field("previous_response_id", &self.previous_response_id)
            .field("sent_fingerprints", &self.sent_fingerprints)
            .finish()
    }
}

impl Conv {
    /// Align the recorded chain with the incoming history and compute this
    /// attempt's skip point and continuation id.
    ///
    /// The recorded fingerprints must be a prefix of `fingerprints`: a
    /// conversation may only extend the history the server already holds.
    /// When the incoming history does not extend it (same first request,
    /// mutated middle, or truncated), the chain is void: it is cleared here
    /// so the turn resends the full input without a continuation id.
    pub(crate) fn plan(
        &mut self,
        fingerprints: &[u64],
        messages: &[ModelRequest],
    ) -> (usize, Option<String>) {
        let extends = self.sent_fingerprints.len() <= fingerprints.len()
            && self
                .sent_fingerprints
                .iter()
                .zip(fingerprints)
                .all(|(sent, incoming)| sent == incoming);
        if !extends {
            tracing::debug!(
                sent = self.sent_fingerprints.len(),
                incoming = fingerprints.len(),
                "history diverged from the sent chain; restarting the conversation"
            );
            self.previous_response_id = None;
            self.sent_fingerprints.clear();
        }
        let chained = self.previous_response_id.is_some();
        let skip = if chained {
            continuation_skip(messages, self.sent_fingerprints.len())
        } else {
            0
        };
        // Invariant: chained was derived from the same Option above.
        let previous = chained.then(|| self.previous_response_id.clone().expect("checked"));
        (skip, previous)
    }
}

/// A conversation whose turns serialize on their own lock.
pub(crate) type SharedConv = Arc<Mutex<Conv>>;

/// Fingerprint a request for in-process conversation keying.
///
/// The hash covers the serialized request parts. `DefaultHasher` is not
/// stable across processes or compiler releases, so fingerprints are
/// in-process only: they key in-memory conversation state and are never
/// persisted nor sent on the wire. Serialization of the parts is
/// deterministic for a given value (serde writes fields in declaration
/// order), and infallible for these serde-derived types, so equal requests
/// always hash equally within a process.
pub(crate) fn fingerprint(request: &ModelRequest) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(&request.parts)
        .expect("request parts are serde types and always serialize")
        .hash(&mut hasher);
    hasher.finish()
}

/// Advance the continuation skip point past the assistant echo.
///
/// After a completed turn the caller appends the response to its local
/// history; with `previous_response_id` chaining the server already has that
/// output, so trailing model-response requests are not re-sent. Only used on
/// chained turns: a fresh session needs the full replay, old assistant
/// output included.
fn continuation_skip(messages: &[ModelRequest], sent: usize) -> usize {
    let mut skip = sent;
    while skip < messages.len()
        && messages[skip]
            .parts
            .iter()
            .all(|part| matches!(part, ModelRequestPart::ModelResponse(_)))
    {
        skip += 1;
    }
    skip
}

/// Where a websocket turn's event stream goes.
///
/// Non-streaming turns collect events for folding; streaming turns forward
/// them through a channel. The sink is async so streaming callers get
/// backpressure instead of dropped events.
#[async_trait]
pub(crate) trait EventSink: Send {
    async fn send(&mut self, event: ModelResponseStreamEvent) -> Result<(), ModelError>;
}

/// Collects events for later folding into a `ModelResponse`.
pub(crate) struct CollectSink(pub(crate) Vec<ModelResponseStreamEvent>);

#[async_trait]
impl EventSink for CollectSink {
    async fn send(&mut self, event: ModelResponseStreamEvent) -> Result<(), ModelError> {
        self.0.push(event);
        Ok(())
    }
}

/// Forwards events to a streaming caller.
pub(crate) struct ChannelSink<'a>(
    pub(crate) &'a mpsc::Sender<Result<ModelResponseStreamEvent, ModelError>>,
);

#[async_trait]
impl EventSink for ChannelSink<'_> {
    async fn send(&mut self, event: ModelResponseStreamEvent) -> Result<(), ModelError> {
        self.0
            .send(Ok(event))
            .await
            .map_err(|_| ModelError::Cancelled)
    }
}

impl super::OpenAIResponsesModel {
    /// Look up or create the conversation state for this history.
    ///
    /// Conversations are keyed by the first request's fingerprint: a
    /// continued history lands in the conversation it extends, a fresh first
    /// request starts its own conversation. The map lock guards lookup and
    /// insert only (no await is performed under it); turn serialization
    /// happens on the conversation's own lock, so different conversations
    /// never wait on each other.
    pub(crate) fn conversation(&self, messages: &[ModelRequest]) -> SharedConv {
        let key = messages
            .first()
            .map_or_else(|| fingerprint(&ModelRequest::new()), fingerprint);
        let mut conversations = self
            .conversations
            .lock()
            .expect("conversations map lock poisoned; it is only held for lookup/insert");
        conversations
            .entry(key)
            .or_insert_with(|| {
                Arc::new(Mutex::new(Conv {
                    #[cfg(feature = "responses-ws")]
                    socket: None,
                    previous_response_id: None,
                    sent_fingerprints: Vec::new(),
                }))
            })
            .clone()
    }
}

#[cfg(all(test, feature = "responses-ws"))]
mod session_state_tests {
    use super::{Conv, Transport, fingerprint};
    use crate::model::ModelRequestParameters;
    use crate::openai::responses::OpenAIResponsesModel;
    use crate::openai::responses::ws::build_request;
    use serdes_ai_core::ModelSettings;
    use serdes_ai_core::messages::{ModelRequest, ModelRequestPart, UserPromptPart};

    fn model() -> OpenAIResponsesModel {
        OpenAIResponsesModel::new("gpt-5.6-luna", "test-key").with_transport(Transport::WebSocket)
    }

    fn user_request(text: &str) -> ModelRequest {
        ModelRequest::with_parts(vec![ModelRequestPart::UserPrompt(UserPromptPart::new(
            text,
        ))])
    }

    #[test]
    fn fingerprints_match_identical_requests_and_split_different_ones() {
        // Equality is by value: a conversation's history re-presents the
        // same request values on every turn (parts carry their creation
        // timestamp), so clones fingerprint alike.
        let request = user_request("a");
        assert_eq!(fingerprint(&request), fingerprint(&request.clone()));
        assert_ne!(
            fingerprint(&request),
            fingerprint(&user_request("b")),
            "different requests must not share a fingerprint"
        );
    }

    #[test]
    fn plan_keeps_the_chain_only_when_history_extends_the_sent_prefix() {
        let mut conv = Conv {
            socket: None,
            previous_response_id: Some("resp_1".to_string()),
            sent_fingerprints: vec![1, 2, 3],
        };

        // An extension chains and skips past the verified prefix.
        let (skip, previous) = conv.plan(&[1, 2, 3, 4], &[]);
        assert_eq!(previous.as_deref(), Some("resp_1"));
        assert_eq!(skip, 3);
        assert_eq!(conv.sent_fingerprints, vec![1, 2, 3]);

        // A truncated or diverging history voids the chain: full replay,
        // no continuation id.
        let mut conv = Conv {
            socket: None,
            previous_response_id: Some("resp_1".to_string()),
            sent_fingerprints: vec![1, 2, 3],
        };
        let (skip, previous) = conv.plan(&[1, 2, 9], &[]);
        assert_eq!(previous, None);
        assert_eq!(skip, 0);
        assert!(conv.previous_response_id.is_none());
        assert!(conv.sent_fingerprints.is_empty());
    }

    #[test]
    fn an_empty_turn_serializes_input_as_a_list() {
        // A chained turn whose history carries nothing new beyond the
        // recorded chain (the caller re-sent exactly what was already
        // delivered) skips every item. The API rejects an empty string
        // here with "Input must be a list", so the empty case has to stay
        // an array.
        let messages: Vec<ModelRequest> = Vec::new();
        let request = build_request(
            &model(),
            &messages,
            &ModelSettings::default(),
            &ModelRequestParameters::default(),
            0,
            Some("resp_1".to_string()),
            false,
        )
        .expect("request");

        let body = serde_json::to_value(&request).expect("serialize");
        assert!(
            body["input"].is_array(),
            "input must be a list when empty, got {}",
            body["input"]
        );
    }
}
