//! Client-side `Model` implementation for the OpenAI Responses protocol.
//!
//! [`OpenResponsesModel`] drives a Responses API endpoint (OpenAI, a codex
//! endpoint, or any Open Responses-compatible server) over websockets or
//! plain HTTP, and keeps conversation state in the session so each turn only
//! sends the new input items.

mod assembler;

use crate::convert::{history_to_wire, tool_choice_to_wire, tool_to_wire};
use crate::error::{codes, WsErrorEnvelope};
use crate::types::{
    CreateResponseRequest, ReasoningSettings, ResponseObject, ResponseStatus, StreamEvent,
};
use async_trait::async_trait;
use serde::Serialize;
use serdes_ai_core::messages::{ModelRequest, ModelRequestPart, ModelResponseStreamEvent};
use serdes_ai_core::FinishReason;
use serdes_ai_core::ModelFailureKind;
use serdes_ai_core::{ModelResponse, ModelSettings, RequestUsage};
use serdes_ai_models::model::{Model, ModelRequestParameters, StreamedResponse};
use serdes_ai_models::profile::{openai_gpt4o_profile, ModelProfile};
use serdes_ai_models::ModelError;
use serdes_ai_streaming::websocket::{WebSocketConfig, WebSocketStream, WsStreamMessage};
use serdes_ai_tools::ToolDefinition;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
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
#[derive(Serialize)]
struct ResponseCreateFrame<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    response: &'a CreateResponseRequest,
}

/// How many times a turn may be retried internally (continuation replay
/// after `previous_response_not_found`, reconnect after a connection
/// failure or the server's connection lifetime limit) before the error
/// surfaces.
const MAX_ATTEMPTS: usize = 3;

/// Connection-local session state.
///
/// The websocket variant keeps the socket alive across turns; conversation
/// state (`previous_response_id`, how many requests were already sent) lives
/// here, which is what makes delta-only continuation turns possible. Turns
/// are sequential: the protocol has no way to match interleaved responses.
struct Session {
    socket: Option<WebSocketStream>,
    previous_response_id: Option<String>,
    sent_requests: usize,
}

struct Inner {
    model_name: String,
    endpoint: String,
    transport: Transport,
    headers: Vec<(String, String)>,
    reasoning: Option<ReasoningSettings>,
    http: reqwest::Client,
    profile: ModelProfile,
    session: Mutex<Session>,
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
                session: Mutex::new(Session {
                    socket: None,
                    previous_response_id: None,
                    sent_requests: 0,
                }),
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
    let mut session = inner.session.lock().await;
    let mut streamed_any = false;

    for _attempt in 0..MAX_ATTEMPTS {
        // Reconnect if needed. A fresh socket means a fresh server-side
        // session, so continuation state from the old socket is void.
        if session.socket.is_none() {
            let mut config = WebSocketConfig::new(inner.endpoint.clone());
            config.headers = inner.headers.clone();
            session.socket = Some(
                WebSocketStream::connect(config)
                    .await
                    .map_err(|e| ModelError::Connection(e.to_string()))?,
            );
            session.previous_response_id = None;
            session.sent_requests = 0;
        }

        let chained = session.previous_response_id.is_some();
        let skip = if chained {
            continuation_skip(messages, session.sent_requests)
        } else {
            0
        };
        let previous = chained.then(|| session.previous_response_id.clone().expect("checked"));
        let request = build_request(inner, messages, settings, params, skip, previous, false)?;
        let frame = ResponseCreateFrame {
            kind: "response.create",
            response: &request,
        };

        let socket = session.socket.as_mut().expect("socket ensured above");
        let mut outcome = match socket.send_json(&frame).await {
            Ok(()) => None,
            Err(e) => Some(if streamed_any {
                AttemptOutcome::Failed(ModelError::Connection(e.to_string()))
            } else {
                AttemptOutcome::Retry(RetryKind::Reconnect)
            }),
        };
        if outcome.is_none() {
            outcome = Some(read_ws_events(socket, sink, &mut streamed_any).await);
        }

        match outcome.expect("outcome set") {
            AttemptOutcome::Finished(response, _reason) => {
                session.previous_response_id = Some(response.id.clone());
                session.sent_requests = messages.len();
                return Ok(*response);
            }
            AttemptOutcome::Retry(RetryKind::StaleContinuation) => {
                session.previous_response_id = None;
                session.sent_requests = 0;
                continue;
            }
            AttemptOutcome::Retry(RetryKind::Reconnect) => {
                session.socket = None;
                continue;
            }
            AttemptOutcome::Failed(error) => return Err(error),
        }
    }

    Err(ModelError::Connection(
        "websocket turn exhausted retries".to_string(),
    ))
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
                return if *streamed_any {
                    AttemptOutcome::Failed(ModelError::Connection(e.to_string()))
                } else {
                    AttemptOutcome::Retry(RetryKind::Reconnect)
                };
            }
            None => {
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
            Err(_) => match serde_json::from_str::<WsErrorEnvelope>(&text) {
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
                Err(_) => {
                    tracing::warn!(frame = %text, "unparseable websocket frame");
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
        input: if items.is_empty() {
            crate::types::ResponseInput::Text(String::new())
        } else {
            crate::types::ResponseInput::Items(items)
        },
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
        let mut session = inner.session.lock().await;

        for _attempt in 0..MAX_ATTEMPTS {
            let chained = session.previous_response_id.is_some();
            let skip = if chained {
                continuation_skip(messages, session.sent_requests)
            } else {
                0
            };
            let previous = chained.then(|| session.previous_response_id.clone().expect("checked"));
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
                    session.previous_response_id = None;
                    session.sent_requests = 0;
                    continue;
                }
                return Err(ModelError::http(status, format!("{code}: {body}")));
            }

            let object: ResponseObject = response
                .json()
                .await
                .map_err(|e| ModelError::InvalidResponse(e.to_string()))?;
            session.previous_response_id = Some(object.id.clone());
            session.sent_requests = messages.len();
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

    let mut session = inner.session.lock().await;

    let response = loop {
        let chained = session.previous_response_id.is_some();
        let skip = if chained {
            continuation_skip(messages, session.sent_requests)
        } else {
            0
        };
        let previous = chained.then(|| session.previous_response_id.clone().expect("checked"));
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
            session.previous_response_id = None;
            session.sent_requests = 0;
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
                session.previous_response_id = Some(response.id.clone());
                session.sent_requests = messages.len();
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
