//! Error types for the Open Responses server.
//!
//! Errors are surfaced in two envelope shapes:
//!
//! - HTTP responses use the OpenAI-style body `{"error": {"code", "message"}}`
//!   with an appropriate status code.
//! - WebSocket frames use the event envelope
//!   `{"type": "error", "status_code", "error": {"code", "message"}}`,
//!   matching what the codex CLI parses (`codex-rs/codex-api` websocket
//!   client and the Open Responses websocket specification).

use serde::{Deserialize, Serialize};

/// Well-known error codes used by this server.
pub mod codes {
    /// Malformed or unsupported request.
    pub const INVALID_REQUEST_ERROR: &str = "invalid_request_error";
    /// Requested resource does not exist.
    pub const NOT_FOUND_ERROR: &str = "not_found_error";
    /// `previous_response_id` references a response this server does not have.
    ///
    /// The Open Responses websocket transport (and the codex CLI) treat this
    /// code as recoverable by replaying the full conversation input.
    pub const PREVIOUS_RESPONSE_NOT_FOUND: &str = "previous_response_not_found";
    /// The backing serdesAI model failed.
    pub const MODEL_ERROR: &str = "model_error";
    /// Unexpected server-side failure.
    pub const INTERNAL_ERROR: &str = "internal_error";
    /// WebSocket connection exceeded its maximum lifetime (Open Responses:
    /// 60 minutes, enforced between turns).
    pub const WEBSOCKET_CONNECTION_LIMIT_REACHED: &str = "websocket_connection_limit_reached";
}

/// Errors produced while handling a Responses API turn.
#[derive(Debug, thiserror::Error)]
pub enum ResponsesError {
    /// The request is malformed or uses an unsupported feature.
    #[error("{0}")]
    InvalidRequest(String),
    /// A stored response with the given ID does not exist.
    #[error("{0}")]
    NotFound(String),
    /// `previous_response_id` is unknown to the store and, on websockets, to
    /// the connection-local session cache.
    #[error("previous response not found: {0}")]
    PreviousResponseNotFound(String),
    /// The backing model request failed.
    #[error("model error: {0}")]
    Model(String),
    /// The websocket connection outlived its allowed lifetime.
    #[error("websocket connection lifetime limit reached")]
    ConnectionLimitReached,
}

impl ResponsesError {
    /// HTTP status code for the error.
    #[must_use]
    pub fn status(&self) -> u16 {
        match self {
            Self::InvalidRequest(_) => 400,
            Self::NotFound(_) => 404,
            Self::PreviousResponseNotFound(_) => 404,
            Self::Model(_) => 502,
            Self::ConnectionLimitReached => 429,
        }
    }

    /// Stable error code for the error.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => codes::INVALID_REQUEST_ERROR,
            Self::NotFound(_) => codes::NOT_FOUND_ERROR,
            Self::PreviousResponseNotFound(_) => codes::PREVIOUS_RESPONSE_NOT_FOUND,
            Self::Model(_) => codes::MODEL_ERROR,
            Self::ConnectionLimitReached => codes::WEBSOCKET_CONNECTION_LIMIT_REACHED,
        }
    }

    /// The `previous_response_id` this error refers to, if any.
    ///
    /// Websocket sessions use this to evict a stale continuation ID after a
    /// failed chain so the client is pushed to replay full input.
    #[must_use]
    pub fn previous_response_id(&self) -> Option<String> {
        match self {
            Self::PreviousResponseNotFound(id) => Some(id.clone()),
            _ => None,
        }
    }

    /// The error body carried by both envelope shapes.
    #[must_use]
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            code: self.code().to_string(),
            message: self.to_string(),
            param: None,
        }
    }
}

/// Error payload embedded in both the HTTP and websocket envelopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// Machine-readable error code. Absent on generic `invalid_request`
    /// frames from the live backend, which identify themselves only by
    /// `type`; the alias maps that field onto `code` so such frames parse.
    #[serde(default, alias = "type")]
    pub code: String,
    /// Human-readable description.
    pub message: String,
    /// Optional parameter the error refers to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
}

/// OpenAI-style HTTP error body: `{"error": {"code", "message"}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpErrorEnvelope {
    /// The error payload.
    pub error: ErrorBody,
}

impl HttpErrorEnvelope {
    /// Build an envelope from an error.
    #[must_use]
    pub fn from_error(err: &ResponsesError) -> Self {
        Self { error: err.body() }
    }
}

/// Websocket error event envelope.
///
/// Shaped as `{"type": "error", "status_code": N, "error": {"code", "message"}}`
/// so codex-style clients can read `status`/`status_code` and the nested code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsErrorEnvelope {
    /// Always `"error"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// HTTP-equivalent status code for the failure.
    ///
    /// The live codex backend names this field `status`; both spellings are
    /// accepted on the wire.
    #[serde(alias = "status")]
    pub status_code: u16,
    /// The error payload.
    pub error: ErrorBody,
}

impl WsErrorEnvelope {
    /// Build an envelope from an error.
    #[must_use]
    pub fn from_error(err: &ResponsesError) -> Self {
        Self {
            kind: "error".to_string(),
            status_code: err.status(),
            error: err.body(),
        }
    }

    /// Serialize to a JSON string (one websocket text frame).
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            "{\"type\":\"error\",\"status_code\":500,\"error\":{\"code\":\"internal_error\",\"message\":\"unserializable error\"}".to_string()
                + "}"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_and_codes() {
        assert_eq!(ResponsesError::InvalidRequest("x".into()).status(), 400);
        assert_eq!(
            ResponsesError::PreviousResponseNotFound("resp_x".into()).status(),
            404
        );
        assert_eq!(
            ResponsesError::PreviousResponseNotFound("resp_x".into()).code(),
            codes::PREVIOUS_RESPONSE_NOT_FOUND
        );
        assert_eq!(ResponsesError::ConnectionLimitReached.status(), 429);
        assert_eq!(
            ResponsesError::ConnectionLimitReached.code(),
            codes::WEBSOCKET_CONNECTION_LIMIT_REACHED
        );
    }

    #[test]
    fn ws_envelope_shape() {
        let err = ResponsesError::Model("boom".into());
        let json = WsErrorEnvelope::from_error(&err).to_json();
        assert!(json.contains("\"type\":\"error\""));
        assert!(json.contains("\"status_code\":502"));
        assert!(json.contains("\"code\":\"model_error\""));
        assert!(json.contains("\"message\":\"model error: boom\""));
    }

    #[test]
    fn http_envelope_shape() {
        let err = ResponsesError::NotFound("no such response".into());
        let json = serde_json::to_string(&HttpErrorEnvelope::from_error(&err)).unwrap();
        assert_eq!(
            json,
            "{\"error\":{\"code\":\"not_found_error\",\"message\":\"no such response\"}}"
        );
    }
}
