//! Client-side `Model` implementation for the OpenAI Responses protocol.
//!
//! [`OpenResponsesModel`] drives a Responses API endpoint (OpenAI, a codex
//! endpoint, or any Open Responses-compatible server) over websockets or
//! plain HTTP, and keeps per-conversation state so each turn only sends the
//! new input items of its own conversation. Conversations are keyed by the
//! first request of the history, so different conversations through one
//! model instance stay isolated and run concurrently.

mod assembler;

use crate::convert::{history_to_wire, tool_choice_to_wire, tool_to_wire};
use crate::error::{WsErrorEnvelope, codes};
use crate::types::{
    CreateResponseRequest, ReasoningSettings, ResponseObject, ResponseStatus, StreamEvent,
};
use async_trait::async_trait;
use serde::Serialize;
use serdes_ai_core::FinishReason;
use serdes_ai_core::ModelFailureKind;
use serdes_ai_core::messages::{ModelRequest, ModelRequestPart, ModelResponseStreamEvent};
use serdes_ai_core::{ModelResponse, ModelSettings, RequestUsage};
use serdes_ai_models::ModelError;
use serdes_ai_models::model::{Model, ModelRequestParameters, StreamedResponse};
use serdes_ai_models::profile::{ModelProfile, openai_gpt4o_profile};
use serdes_ai_streaming::websocket::{WebSocketConfig, WebSocketStream, WsStreamMessage};
use serdes_ai_tools::ToolDefinition;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;

/// Transport used to reach the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// Websocket (`wss://`/`ws://`): session-stateful turns with
    /// `store: false` and delta-only input. This is the transport the codex
    /// CLI and Open Responses servers are designed around.
    #[default]
    WebSocket,
    /// HTTP (`https://`/`http://`): stateful chaining via `store: true` and
    /// `previous_response_id` on each `POST`.
    Http,
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

/// The wire frame that initiates a websocket turn.
///
/// The codex wire form is flat: `type` plus the response parameters at the
/// top level (`{"type":"response.create","model":…,"input":…}`), with no
/// `response` wrapper. The live backend reads `model` from the frame root
/// and reports `None` when it is nested.
#[derive(Serialize)]
struct ResponseCreateFrame<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(flatten)]
    response: &'a CreateResponseRequest,
}

/// How many times a turn may be retried internally (continuation replay
/// after `previous_response_not_found`, reconnect after a connection
/// failure or the server's connection lifetime limit) before the error
/// surfaces.
const MAX_ATTEMPTS: usize = 3;

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
struct Conv {
    socket: Option<WebSocketStream>,
    previous_response_id: Option<String>,
    /// Fingerprints of the requests the server already holds, in order. A
    /// turn's history must extend this sequence exactly; anything else
    /// voids the chain and forces a full replay.
    sent_fingerprints: Vec<u64>,
}

/// A conversation whose turns serialize on their own lock.
type SharedConv = Arc<Mutex<Conv>>;

struct Inner {
    model_name: String,
    endpoint: String,
    transport: Transport,
    headers: Vec<(String, String)>,
    reasoning: Option<ReasoningSettings>,
    http: reqwest::Client,
    profile: ModelProfile,
    /// Conversation state, keyed by first-request fingerprint. The map lock
    /// guards lookup and insert only (no await is performed under it);
    /// each conversation serializes its own turns on its lock.
    conversations: parking_lot::Mutex<HashMap<u64, SharedConv>>,
}

/// A serdesAI [`Model`] that talks to an OpenAI Responses API endpoint.
///
/// ```no_run
/// # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
/// use serdes_ai_responses::client::OpenResponsesModel;
///
/// // Any Open Responses-compatible websocket endpoint:
/// let model = OpenResponsesModel::new("gpt-5.1-codex-mini", "wss://host/v1/responses")
///     .bearer("sk-…");
///
/// // Or the codex endpoint over HTTP:
/// let model = OpenResponsesModel::new(
///     "gpt-5.1-codex-mini",
///     "https://chatgpt.com/backend-api/codex/responses",
/// )
/// .bearer("oauth-token")
/// .header("chatgpt-account-id", "…");
/// # let _ = (&model, &model);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct OpenResponsesModel {
    inner: Arc<Inner>,
}

/// Where a websocket turn's event stream goes.
///
/// Non-streaming turns collect events for folding; streaming turns forward
/// them through a channel. The sink is async so streaming callers get
/// backpressure instead of dropped events.
#[async_trait]
trait EventSink: Send {
    async fn send(&mut self, event: ModelResponseStreamEvent) -> Result<(), ModelError>;
}

/// Collects events for later folding into a `ModelResponse`.
struct CollectSink(Vec<ModelResponseStreamEvent>);

#[async_trait]
impl EventSink for CollectSink {
    async fn send(&mut self, event: ModelResponseStreamEvent) -> Result<(), ModelError> {
        self.0.push(event);
        Ok(())
    }
}

/// Forwards events to a streaming caller.
struct ChannelSink<'a>(&'a mpsc::Sender<Result<ModelResponseStreamEvent, ModelError>>);

#[async_trait]
impl EventSink for ChannelSink<'_> {
    async fn send(&mut self, event: ModelResponseStreamEvent) -> Result<(), ModelError> {
        self.0
            .send(Ok(event))
            .await
            .map_err(|_| ModelError::Cancelled)
    }
}

impl OpenResponsesModel {
    /// Create a client for `model_name` at `endpoint` (a full `wss://` or
    /// `https://` responses URL). The transport is inferred from the scheme.
    #[must_use]
    pub fn new(model_name: impl Into<String>, endpoint: impl Into<String>) -> Self {
        let model_name = model_name.into();
        let endpoint = endpoint.into();
        let transport = Transport::from_url(&endpoint);
        Self {
            inner: Arc::new(Inner {
                model_name,
                endpoint,
                transport,
                headers: Vec::new(),
                reasoning: None,
                http: reqwest::Client::new(),
                profile: openai_gpt4o_profile(),
                conversations: parking_lot::Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Override the transport inferred from the URL scheme.
    ///
    /// Builder methods must be called before the model is shared (used to
    /// run turns); they panic otherwise.
    #[must_use]
    pub fn with_transport(mut self, transport: Transport) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("model already in use; configure before sharing")
            .transport = transport;
        self
    }

    /// Authenticate with a bearer token (HTTP `Authorization` header, or the
    /// same header on the websocket handshake).
    #[must_use]
    pub fn bearer(mut self, token: impl Into<String>) -> Self {
        let value = format!("Bearer {}", token.into());
        let inner =
            Arc::get_mut(&mut self.inner).expect("model already in use; configure before sharing");
        inner.headers.retain(|(name, _)| name != "Authorization");
        inner.headers.push(("Authorization".to_string(), value));
        self
    }

    /// Add an arbitrary header (websocket handshake or HTTP request).
    #[must_use]
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        let inner =
            Arc::get_mut(&mut self.inner).expect("model already in use; configure before sharing");
        inner.headers.push((name.into(), value.into()));
        self
    }

    /// Configure reasoning (effort/summary) for every turn.
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: ReasoningSettings) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("model already in use; configure before sharing")
            .reasoning = Some(reasoning);
        self
    }

    /// Use a custom HTTP client (HTTP transport only).
    #[must_use]
    pub fn with_http_client(mut self, client: reqwest::Client) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("model already in use; configure before sharing")
            .http = client;
        self
    }
}

/// Outcome of one websocket attempt.
enum AttemptOutcome {
    /// The turn reached a terminal event; carries the final response object.
    Finished(Box<ResponseObject>, FinishReason),
    /// Recoverable before any event escaped; retry with adjusted session.
    Retry(RetryKind),
    /// Terminal failure; carries the error to surface.
    Failed(ModelError),
}

/// Recoverable failure modes.
enum RetryKind {
    /// Stale `previous_response_id`: clear continuation, replay everything.
    StaleContinuation,
    /// Socket is dead or rejected: reconnect and replay.
    Reconnect,
}

/// Fingerprint a request for in-process conversation keying.
///
/// The hash covers the serialized request parts. `DefaultHasher` is not
/// stable across processes or compiler releases, so fingerprints are
/// in-process only: they key in-memory conversation state and are never
/// persisted nor sent on the wire. Serialization of the parts is
/// deterministic for a given value (serde writes fields in declaration
/// order), and infallible for these serde-derived types, so equal requests
/// always hash equally within a process.
fn fingerprint(request: &ModelRequest) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(&request.parts)
        .expect("request parts are serde types and always serialize")
        .hash(&mut hasher);
    hasher.finish()
}

/// Look up or create the conversation state for this history.
///
/// Conversations are keyed by the first request's fingerprint: a continued
/// history lands in the conversation it extends, a fresh first request
/// starts its own conversation. The map lock is held only for the lookup;
/// turn serialization happens on the conversation's own lock, so different
/// conversations never wait on each other.
fn conversation(inner: &Inner, messages: &[ModelRequest]) -> SharedConv {
    let key = messages
        .first()
        .map_or_else(|| fingerprint(&ModelRequest::new()), fingerprint);
    let mut conversations = inner.conversations.lock();
    conversations
        .entry(key)
        .or_insert_with(|| {
            Arc::new(Mutex::new(Conv {
                socket: None,
                previous_response_id: None,
                sent_fingerprints: Vec::new(),
            }))
        })
        .clone()
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

impl Conv {
    /// Align the recorded chain with the incoming history and compute this
    /// attempt's skip point and continuation id.
    ///
    /// The recorded fingerprints must be a prefix of `fingerprints`: a
    /// conversation may only extend the history the server already holds.
    /// When the incoming history does not extend it (same first request,
    /// mutated middle, or truncated), the chain is void: it is cleared here
    /// so the turn resends the full input without a continuation id.
    fn plan(&mut self, fingerprints: &[u64], messages: &[ModelRequest]) -> (usize, Option<String>) {
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

/// Run one turn over the websocket transport.
///
/// `sink` receives every model event; events are only emitted once the turn
/// is committed (never across internal retries), so a caller-visible event
/// implies no further replay. Returns the final response object.
async fn run_ws_turn(
    inner: &Inner,
    messages: &[ModelRequest],
    settings: &ModelSettings,
    params: &ModelRequestParameters,
    sink: &mut dyn EventSink,
) -> Result<ResponseObject, ModelError> {
    let fingerprints: Vec<u64> = messages.iter().map(fingerprint).collect();
    let conv = conversation(inner, messages);
    let mut state = conv.lock().await;
    let mut streamed_any = false;
    let mut last_cause: Option<String> = None;

    for _attempt in 0..MAX_ATTEMPTS {
        // Reconnect if needed. A fresh socket means a fresh server-side
        // session, so continuation state from the old socket is void.
        if state.socket.is_none() {
            let mut config = WebSocketConfig::new(inner.endpoint.clone());
            config.headers = inner.headers.clone();
            state.socket = Some(
                WebSocketStream::connect(config)
                    .await
                    .map_err(|e| ModelError::Connection(e.to_string()))?,
            );
            state.previous_response_id = None;
            state.sent_fingerprints.clear();
        }

        let (skip, previous) = state.plan(&fingerprints, messages);
        let request = build_request(inner, messages, settings, params, skip, previous, false)?;
        let frame = ResponseCreateFrame {
            kind: "response.create",
            response: &request,
        };
        tracing::debug!(frame = %serde_json::to_string(&frame).unwrap_or_default(), "sending response.create");

        let socket = state.socket.as_mut().expect("socket ensured above");
        let mut outcome = match socket.send_json(&frame).await {
            Ok(()) => None,
            Err(e) => Some(if streamed_any {
                AttemptOutcome::Failed(ModelError::Connection(e.to_string()))
            } else {
                last_cause = Some(e.to_string());
                tracing::warn!(error = %e, "send failed before any event; reconnecting");
                AttemptOutcome::Retry(RetryKind::Reconnect)
            }),
        };
        if outcome.is_none() {
            outcome = Some(read_ws_events(socket, sink, &mut streamed_any).await);
        }

        match outcome.expect("outcome set") {
            AttemptOutcome::Finished(response, _reason) => {
                state.previous_response_id = Some(response.id.clone());
                state.sent_fingerprints = fingerprints.clone();
                return Ok(*response);
            }
            AttemptOutcome::Retry(RetryKind::StaleContinuation) => {
                state.previous_response_id = None;
                state.sent_fingerprints.clear();
                continue;
            }
            AttemptOutcome::Retry(RetryKind::Reconnect) => {
                state.socket = None;
                continue;
            }
            AttemptOutcome::Failed(error) => return Err(error),
        }
    }

    Err(ModelError::Connection(match last_cause {
        Some(cause) => {
            format!("websocket turn exhausted retries; last cause: {cause}")
        }
        None => "websocket turn exhausted retries".to_string(),
    }))
}

/// Read frames for one attempt, translating events into the sink until the
/// turn reaches a terminal event, a recoverable error, or a failure. The
/// caller owns the session and applies the retry adjustment.
async fn read_ws_events(
    socket: &mut WebSocketStream,
    sink: &mut dyn EventSink,
    streamed_any: &mut bool,
) -> AttemptOutcome {
    loop {
        let message = match socket.next_message().await {
            Some(Ok(message)) => message,
            Some(Err(e)) => {
                if !*streamed_any {
                    tracing::warn!(error = %e, "socket error before any event; reconnecting");
                }
                return if *streamed_any {
                    AttemptOutcome::Failed(ModelError::Connection(e.to_string()))
                } else {
                    AttemptOutcome::Retry(RetryKind::Reconnect)
                };
            }
            None => {
                if !*streamed_any {
                    tracing::warn!("socket closed by peer before any event; reconnecting");
                }
                return if *streamed_any {
                    AttemptOutcome::Failed(ModelError::Connection(
                        "connection closed mid-turn".to_string(),
                    ))
                } else {
                    AttemptOutcome::Retry(RetryKind::Reconnect)
                };
            }
        };
        let text = match message {
            WsStreamMessage::Text(text) => text,
            WsStreamMessage::Close => {
                if !*streamed_any {
                    tracing::warn!("close frame before any event; reconnecting");
                }
                return if *streamed_any {
                    AttemptOutcome::Failed(ModelError::Connection(
                        "connection closed mid-turn".to_string(),
                    ))
                } else {
                    AttemptOutcome::Retry(RetryKind::Reconnect)
                };
            }
            WsStreamMessage::Ping | WsStreamMessage::Pong | WsStreamMessage::Binary(_) => continue,
        };

        let event = match serde_json::from_str::<StreamEvent>(&text) {
            Ok(event) => event,
            Err(event_error) => match serde_json::from_str::<WsErrorEnvelope>(&text) {
                Ok(envelope) => {
                    let code = envelope.error.code.as_str();
                    if code == codes::PREVIOUS_RESPONSE_NOT_FOUND && !*streamed_any {
                        return AttemptOutcome::Retry(RetryKind::StaleContinuation);
                    }
                    if code == codes::WEBSOCKET_CONNECTION_LIMIT_REACHED && !*streamed_any {
                        return AttemptOutcome::Retry(RetryKind::Reconnect);
                    }
                    return AttemptOutcome::Failed(envelope_error(&envelope));
                }
                Err(envelope_error) => {
                    tracing::warn!(frame = %text, event_error = %event_error, envelope_error = %envelope_error, "unparseable websocket frame");
                    continue;
                }
            },
        };

        // Capture the terminal response object before translation consumes
        // the event; the terminal model event must be the last one sent.
        let mut terminal: Option<(ResponseObject, FinishReason)> = None;
        match &event {
            StreamEvent::ResponseCompleted { response, .. } => {
                terminal = Some((response.clone(), FinishReason::Stop));
            }
            StreamEvent::ResponseIncomplete { response, .. } => {
                terminal = Some((response.clone(), FinishReason::Length));
            }
            StreamEvent::ResponseFailed { response, .. } => {
                return AttemptOutcome::Failed(assembler::failure(response));
            }
            _ => {}
        }

        for translated in assembler::translate(event) {
            match translated {
                Ok(event) => {
                    if sink.send(event).await.is_err() {
                        return AttemptOutcome::Failed(ModelError::Cancelled);
                    }
                    *streamed_any = true;
                }
                Err(error) => return AttemptOutcome::Failed(error),
            }
        }

        if let Some((response, reason)) = terminal {
            return AttemptOutcome::Finished(Box::new(response), reason);
        }
    }
}
/// Build the request body for a turn.
fn build_request(
    inner: &Inner,
    messages: &[ModelRequest],
    settings: &ModelSettings,
    params: &ModelRequestParameters,
    skip: usize,
    previous_response_id: Option<String>,
    store: bool,
) -> Result<CreateResponseRequest, ModelError> {
    let (instructions, items) = history_to_wire(messages, skip).map_err(client_error)?;
    let tools: Option<Vec<_>> = if params.tools.is_empty() {
        None
    } else {
        Some(
            params
                .tools
                .iter()
                .map(|tool: &ToolDefinition| tool_to_wire(tool))
                .collect(),
        )
    };
    Ok(CreateResponseRequest {
        model: inner.model_name.clone(),
        // Always a list, including when empty. A chained turn whose new
        // items are all skipped has nothing left to add, and the API rejects
        // the empty string an untagged Text variant serializes to with
        // "Input must be a list".
        input: crate::types::ResponseInput::Items(items),
        instructions,
        tools,
        tool_choice: tool_choice_to_wire(params.tool_choice.as_ref()),
        temperature: settings.temperature,
        top_p: settings.top_p,
        max_output_tokens: settings.max_tokens,
        stream: None,
        background: None,
        store: Some(store),
        previous_response_id,
        reasoning: inner.reasoning.clone(),
        parallel_tool_calls: settings.parallel_tool_calls,
        metadata: None,
        user: None,
        truncation: None,
        include: None,
        text: None,
        service_tier: None,
    })
}

/// Map a protocol error onto a model error.
fn client_error(error: crate::error::ResponsesError) -> ModelError {
    ModelError::provider(
        "open-responses",
        error.code(),
        error.to_string(),
        failure_kind(error.code()),
        None,
    )
}

/// Map an error envelope onto a model error.
fn envelope_error(envelope: &WsErrorEnvelope) -> ModelError {
    ModelError::provider(
        "open-responses",
        envelope.error.code.clone(),
        envelope.error.message.clone(),
        failure_kind(&envelope.error.code),
        None,
    )
}

/// Classify a wire error code for retry/fallback policies.
pub(super) fn failure_kind(code: &str) -> ModelFailureKind {
    match code {
        codes::WEBSOCKET_CONNECTION_LIMIT_REACHED => ModelFailureKind::RateLimited,
        codes::NOT_FOUND_ERROR | codes::PREVIOUS_RESPONSE_NOT_FOUND => ModelFailureKind::NotFound,
        codes::INVALID_REQUEST_ERROR => ModelFailureKind::InvalidRequest,
        _ => ModelFailureKind::Server,
    }
}

#[async_trait]
impl Model for OpenResponsesModel {
    fn name(&self) -> &str {
        &self.inner.model_name
    }

    fn system(&self) -> &str {
        "open-responses"
    }

    async fn request(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<ModelResponse, ModelError> {
        match self.inner.transport {
            Transport::WebSocket => {
                let mut sink = CollectSink(Vec::new());
                let response =
                    run_ws_turn(&self.inner, messages, settings, params, &mut sink).await?;
                Ok(response_from_events(
                    sink.0,
                    &self.inner.model_name,
                    &response.id,
                ))
            }
            Transport::Http => self.run_http_turn(messages, settings, params).await,
        }
    }

    async fn request_stream(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<StreamedResponse, ModelError> {
        let (tx, rx) = mpsc::channel::<Result<ModelResponseStreamEvent, ModelError>>(64);
        let inner = Arc::clone(&self.inner);
        let messages: Vec<ModelRequest> = messages.to_vec();
        let settings = settings.clone();
        let params = params.clone();

        tokio::spawn(async move {
            let result = match inner.transport {
                Transport::WebSocket => {
                    let mut sink = ChannelSink(&tx);
                    run_ws_turn(&inner, &messages, &settings, &params, &mut sink)
                        .await
                        .map(|_| ())
                }
                Transport::Http => run_http_stream(&inner, &messages, &settings, &params, &tx)
                    .await
                    .map(|_| ()),
            };
            if let Err(error) = result {
                // A failure after events escaped still reaches the caller as
                // an error item; a failure before that is the only item.
                let _ = tx.try_send(Err(error));
            }
        });

        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn profile(&self) -> &ModelProfile {
        &self.inner.profile
    }
}

/// Fold collected stream events into a complete response.
fn response_from_events(
    events: Vec<ModelResponseStreamEvent>,
    model_name: &str,
    response_id: &str,
) -> ModelResponse {
    use serdes_ai_core::messages::ModelResponsePartDelta;

    let mut parts: Vec<serdes_ai_core::messages::ModelResponsePart> = Vec::new();
    let mut finish_reason = None;
    let mut usage = None;

    for event in events {
        match event {
            ModelResponseStreamEvent::PartStart(start) => {
                if start.index < parts.len() {
                    parts[start.index] = start.part;
                } else {
                    parts.push(start.part);
                }
            }
            ModelResponseStreamEvent::PartDelta(delta) => {
                if let Some(part) = parts.get_mut(delta.index) {
                    match delta.delta {
                        ModelResponsePartDelta::Text(_)
                        | ModelResponsePartDelta::ToolCall(_)
                        | ModelResponsePartDelta::Thinking(_)
                        | ModelResponsePartDelta::BuiltinToolCall(_) => {
                            let _ = delta.delta.apply(part);
                        }
                    }
                }
            }
            ModelResponseStreamEvent::PartEnd(_) => {}
            ModelResponseStreamEvent::StreamComplete(complete) => {
                finish_reason = Some(complete.finish_reason);
                usage = Some(RequestUsage {
                    request_tokens: complete.input_tokens,
                    response_tokens: complete.output_tokens,
                    total_tokens: match (complete.input_tokens, complete.output_tokens) {
                        (Some(input), Some(output)) => Some(input + output),
                        (input, output) => input.or(output),
                    },
                    cache_creation_tokens: complete.cache_creation_tokens,
                    cache_read_tokens: complete.cache_read_tokens,
                    details: None,
                });
            }
        }
    }

    ModelResponse {
        parts,
        model_name: Some(model_name.to_string()),
        timestamp: chrono::Utc::now(),
        finish_reason,
        usage,
        vendor_id: Some(response_id.to_string()),
        vendor_details: None,
        kind: "response".to_string(),
    }
}

impl OpenResponsesModel {
    /// Non-streaming HTTP turn with `store: true` chaining.
    async fn run_http_turn(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<ModelResponse, ModelError> {
        let inner = &self.inner;
        let fingerprints: Vec<u64> = messages.iter().map(fingerprint).collect();
        let conv = conversation(inner, messages);
        let mut state = conv.lock().await;

        for _attempt in 0..MAX_ATTEMPTS {
            let (skip, previous) = state.plan(&fingerprints, messages);
            let chained = previous.is_some();
            let mut request =
                build_request(inner, messages, settings, params, skip, previous, true)?;
            request.stream = Some(false);

            let mut http = inner.http.post(&inner.endpoint).json(&request);
            for (name, value) in &inner.headers {
                http = http.header(name, value);
            }
            let response = http
                .send()
                .await
                .map_err(|e| ModelError::Connection(e.to_string()))?;

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                let code = serde_json::from_str::<crate::error::HttpErrorEnvelope>(&body)
                    .ok()
                    .map(|envelope| envelope.error.code)
                    .unwrap_or_default();
                if code == codes::PREVIOUS_RESPONSE_NOT_FOUND && chained {
                    state.previous_response_id = None;
                    state.sent_fingerprints.clear();
                    continue;
                }
                return Err(ModelError::http(status, format!("{code}: {body}")));
            }

            let object: ResponseObject = response
                .json()
                .await
                .map_err(|e| ModelError::InvalidResponse(e.to_string()))?;
            state.previous_response_id = Some(object.id.clone());
            state.sent_fingerprints = fingerprints.clone();
            return Ok(ModelResponse {
                parts: crate::convert::parts_from_output(&object.output),
                model_name: Some(inner.model_name.clone()),
                timestamp: chrono::Utc::now(),
                finish_reason: Some(match object.status {
                    ResponseStatus::Incomplete => FinishReason::Length,
                    ResponseStatus::Failed => FinishReason::Error,
                    _ => FinishReason::Stop,
                }),
                usage: object.usage.map(|usage| RequestUsage {
                    request_tokens: usage.input_tokens,
                    response_tokens: usage.output_tokens,
                    total_tokens: usage.total_tokens,
                    cache_creation_tokens: None,
                    cache_read_tokens: None,
                    details: None,
                }),
                vendor_id: Some(object.id),
                vendor_details: None,
                kind: "response".to_string(),
            });
        }

        Err(ModelError::Connection(
            "http turn exhausted retries".to_string(),
        ))
    }
}

/// Streaming HTTP turn (SSE).
///
/// The request is established before any event escapes, so a
/// `previous_response_not_found` on the status line clears the stale chain
/// and replays the full input once, mirroring the non-streaming path. Once
/// the SSE body is being read there is no further replay: replaying after
/// events escaped would duplicate output.
async fn run_http_stream(
    inner: &Inner,
    messages: &[ModelRequest],
    settings: &ModelSettings,
    params: &ModelRequestParameters,
    tx: &mpsc::Sender<Result<ModelResponseStreamEvent, ModelError>>,
) -> Result<(), ModelError> {
    use futures::StreamExt;

    let fingerprints: Vec<u64> = messages.iter().map(fingerprint).collect();
    let conv = conversation(inner, messages);
    let mut state = conv.lock().await;

    let response = loop {
        let (skip, previous) = state.plan(&fingerprints, messages);
        let chained = previous.is_some();
        let mut request = build_request(inner, messages, settings, params, skip, previous, true)?;
        request.stream = Some(true);

        let mut http = inner.http.post(&inner.endpoint).json(&request);
        for (name, value) in &inner.headers {
            http = http.header(name, value);
        }
        let response = http
            .send()
            .await
            .map_err(|e| ModelError::Connection(e.to_string()))?;
        if response.status().is_success() {
            break response;
        }
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        let code = serde_json::from_str::<crate::error::HttpErrorEnvelope>(&body)
            .ok()
            .map(|envelope| envelope.error.code)
            .unwrap_or_default();
        if code == codes::PREVIOUS_RESPONSE_NOT_FOUND && chained {
            state.previous_response_id = None;
            state.sent_fingerprints.clear();
            continue;
        }
        return Err(ModelError::http(status, format!("{code}: {body}")));
    };

    let mut byte_stream = response.bytes_stream();
    let mut buffer = String::new();
    while let Some(chunk) = byte_stream.next().await {
        let chunk = chunk.map_err(|e| ModelError::Connection(e.to_string()))?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(newline) = buffer.find('\n') {
            let line: String = buffer.drain(..=newline).collect();
            let payload = line.trim_end_matches(['\n', '\r']);
            let Some(payload) = payload.strip_prefix("data: ") else {
                continue;
            };
            if payload == "[DONE]" {
                return Ok(());
            }
            let event: StreamEvent = serde_json::from_str(payload)
                .map_err(|e| ModelError::InvalidResponse(e.to_string()))?;
            if let StreamEvent::ResponseCompleted { response, .. }
            | StreamEvent::ResponseIncomplete { response, .. } = &event
            {
                state.previous_response_id = Some(response.id.clone());
                state.sent_fingerprints = fingerprints.clone();
            }
            for translated in assembler::translate(event) {
                match translated {
                    Ok(event) => {
                        tx.send(Ok(event))
                            .await
                            .map_err(|_| ModelError::Cancelled)?;
                    }
                    Err(error) => {
                        // Error delivered as a stream item; returning Ok
                        // avoids the task re-sending it.
                        let _ = tx.send(Err(error)).await;
                        return Ok(());
                    }
                }
            }
        }
    }

    Err(ModelError::InvalidResponse(
        "sse stream ended without a terminal event".to_string(),
    ))
}

#[cfg(test)]
mod session_state_tests {
    use super::{Conv, Inner, Transport, build_request, fingerprint};
    use serdes_ai_core::ModelSettings;
    use serdes_ai_core::messages::request::ModelRequest;
    use serdes_ai_core::messages::{ModelRequestPart, UserPromptPart};
    use serdes_ai_models::model::ModelRequestParameters;
    use std::collections::HashMap;

    fn inner() -> Inner {
        Inner {
            model_name: "gpt-5.6-luna".to_string(),
            endpoint: "wss://example.invalid/responses".to_string(),
            transport: Transport::WebSocket,
            headers: Vec::new(),
            reasoning: None,
            http: reqwest::Client::new(),
            profile: Default::default(),
            conversations: parking_lot::Mutex::new(HashMap::new()),
        }
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
            &inner(),
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
