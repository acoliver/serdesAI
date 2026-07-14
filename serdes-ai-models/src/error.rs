//! Model-related error types.

use serdes_ai_core::errors::{ModelApiError, ModelHttpError};
use serdes_ai_core::{ClassifyModelFailure, ModelFailure, ModelFailureKind};
use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;

/// Backward-compatible name for the canonical model failure category.
pub type ProviderErrorKind = ModelFailureKind;

/// Model-related errors.
#[derive(Debug, Error)]
pub enum ModelError {
    /// HTTP error from the API.
    #[error("HTTP error: {status} - {body}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body.
        body: String,
        /// Response headers.
        headers: HashMap<String, String>,
    },

    /// API-level error.
    #[error("API error: {message}")]
    Api {
        /// Error message.
        message: String,
        /// Error code.
        code: Option<String>,
    },

    /// Structured error reported by a model provider.
    #[error("{provider} API error ({code}): {message}")]
    Provider {
        /// Provider name.
        provider: String,
        /// Provider-specific error type or code.
        code: String,
        /// Provider-supplied error message.
        message: String,
        /// Semantic classification used by retry and fallback policies.
        kind: ProviderErrorKind,
        /// HTTP status for transport-reported errors.
        status: Option<u16>,
        /// Suggested retry delay, when supplied by the provider.
        retry_after: Option<Duration>,
    },

    /// Request timeout.
    #[error("Request timeout after {0:?}")]
    Timeout(Duration),

    /// Rate limited by the API.
    #[error("Rate limited, retry after {retry_after:?}")]
    RateLimited {
        /// Suggested retry delay.
        retry_after: Option<Duration>,
    },

    /// Authentication failed.
    #[error("Authentication failed: {0}")]
    Authentication(String),

    /// Invalid response from the API.
    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    /// Model not found.
    #[error("Model not found: {0}")]
    NotFound(String),

    /// Feature not supported by the model.
    #[error("Feature not supported: {0}")]
    NotSupported(String),

    /// JSON serialization/deserialization error.
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Request cancelled.
    #[error("Request cancelled")]
    Cancelled,

    /// Connection error.
    #[error("Connection error: {0}")]
    Connection(String),

    /// Content filter triggered.
    #[error("Content filtered: {0}")]
    ContentFiltered(String),

    /// Context length exceeded.
    #[error("Context length exceeded: {max_tokens} tokens max, got {requested_tokens}")]
    ContextLengthExceeded {
        /// Maximum allowed tokens.
        max_tokens: u64,
        /// Requested tokens.
        requested_tokens: u64,
    },

    /// Configuration error.
    #[error("Configuration error: {0}")]
    Configuration(String),

    /// Stream ended prematurely (e.g. transport EOF before a terminal event).
    #[error("Incomplete stream: {0}")]
    IncompleteStream(String),

    /// Network error.
    #[error("Network error: {0}")]
    Network(String),

    /// Every eligible model in a fallback chain failed before producing output.
    #[error("All fallback models failed after {attempts_len} attempts: {last_error}", attempts_len = .attempts.len())]
    FallbackExhausted {
        /// Normalized failure metadata for each attempted model.
        attempts: Vec<ModelFailure>,
        /// Final concrete error retained as the source.
        #[source]
        last_error: Box<ModelError>,
    },

    /// Retry attempts against the same model were exhausted.
    #[error("Model request failed after {attempts} attempts over {elapsed:?}: {last_error}")]
    RetryExhausted {
        /// Number of attempts made.
        attempts: u32,
        /// Total time spent under the retry policy.
        elapsed: Duration,
        /// Final classified model error.
        #[source]
        last_error: Box<ModelError>,
    },

    /// The total retry deadline expired.
    #[error("Model request deadline expired after {attempts} attempts over {elapsed:?}")]
    RetryDeadlineExceeded {
        /// Number of attempts started.
        attempts: u32,
        /// Total time spent under the retry policy.
        elapsed: Duration,
        /// Most recent classified model error, if any.
        #[source]
        last_error: Option<Box<ModelError>>,
    },

    /// Legacy core API error retained as the source during migration.
    #[error("Core model API error: {0}")]
    CoreApi(#[from] ModelApiError),

    /// Legacy core HTTP error retained as the source during migration.
    #[error("Core model HTTP error: {0}")]
    CoreHttp(#[from] ModelHttpError),

    /// Other error.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ModelError {
    /// Check if this error is retryable.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.model_failure().is_retryable()
    }

    /// Check if this error represents rate limiting.
    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        self.model_failure().kind.is_rate_limited()
    }

    /// Check if this error is transient, excluding rate limits.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        self.model_failure().kind.is_transient()
    }

    /// Get the retry-after duration if applicable.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ModelError::RateLimited { retry_after } => *retry_after,
            ModelError::Provider { retry_after, .. } => *retry_after,
            ModelError::Http { headers, .. } => headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                .and_then(|(_, value)| value.parse::<u64>().ok())
                .map(Duration::from_secs),
            ModelError::FallbackExhausted { last_error, .. }
            | ModelError::RetryExhausted { last_error, .. } => last_error.retry_after(),
            ModelError::RetryDeadlineExceeded {
                last_error: Some(last_error),
                ..
            } => last_error.retry_after(),
            _ => None,
        }
    }

    /// Create an API error.
    pub fn api(message: impl Into<String>) -> Self {
        Self::Api {
            message: message.into(),
            code: None,
        }
    }

    /// Create an API error with code.
    pub fn api_with_code(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self::Api {
            message: message.into(),
            code: Some(code.into()),
        }
    }

    /// Create a rate limited error.
    pub fn rate_limited(retry_after: Option<Duration>) -> Self {
        Self::RateLimited { retry_after }
    }

    /// Create a structured provider error.
    pub fn provider(
        provider: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
        kind: ProviderErrorKind,
        retry_after: Option<Duration>,
    ) -> Self {
        Self::provider_with_status(provider, code, message, kind, None, retry_after)
    }

    /// Create a structured provider error with HTTP status metadata.
    pub fn provider_with_status(
        provider: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
        kind: ProviderErrorKind,
        status: Option<u16>,
        retry_after: Option<Duration>,
    ) -> Self {
        Self::Provider {
            provider: provider.into(),
            code: code.into(),
            message: message.into(),
            kind,
            status,
            retry_after,
        }
    }

    /// Create an HTTP error.
    pub fn http(status: u16, body: impl Into<String>) -> Self {
        Self::Http {
            status,
            body: body.into(),
            headers: HashMap::new(),
        }
    }

    /// Create an HTTP error with headers.
    pub fn http_with_headers(
        status: u16,
        body: impl Into<String>,
        headers: HashMap<String, String>,
    ) -> Self {
        Self::Http {
            status,
            body: body.into(),
            headers,
        }
    }

    /// Create an authentication error.
    pub fn auth(message: impl Into<String>) -> Self {
        Self::Authentication(message.into())
    }

    /// Create an invalid response error.
    pub fn invalid_response(message: impl Into<String>) -> Self {
        Self::InvalidResponse(message.into())
    }

    /// Create an incomplete-stream error.
    pub fn incomplete_stream(message: impl Into<String>) -> Self {
        Self::IncompleteStream(message.into())
    }

    /// Create a not supported error.
    pub fn not_supported(message: impl Into<String>) -> Self {
        Self::NotSupported(message.into())
    }

    /// Create an unsupported content error.
    pub fn unsupported_content(content: impl Into<String>) -> Self {
        Self::NotSupported(format!("Unsupported content type: {}", content.into()))
    }

    /// Create a configuration error.
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration(message.into())
    }

    /// Create a network error.
    pub fn network(message: impl Into<String>) -> Self {
        Self::Network(message.into())
    }

    /// Create an API error with status code.
    pub fn api_error(status_code: u16, message: impl Into<String>) -> Self {
        Self::Http {
            status: status_code,
            body: message.into(),
            headers: std::collections::HashMap::new(),
        }
    }
}
impl ClassifyModelFailure for ModelError {
    fn model_failure(&self) -> ModelFailure {
        let mut failure = match self {
            Self::Http { status, body, .. } => {
                let kind = match *status {
                    429 => ModelFailureKind::RateLimited,
                    500..=599 => ModelFailureKind::Server,
                    400 => ModelFailureKind::InvalidRequest,
                    401 => ModelFailureKind::Authentication,
                    403 => ModelFailureKind::PermissionDenied,
                    404 => ModelFailureKind::NotFound,
                    _ => ModelFailureKind::Other,
                };
                let mut failure = ModelFailure::new(kind, body.clone());
                failure.status = Some(*status);
                failure
            }
            Self::Api { message, code } => {
                let kind = match code.as_deref() {
                    Some("rate_limit_error") => ModelFailureKind::RateLimited,
                    Some("overloaded_error") => ModelFailureKind::Overloaded,
                    Some("authentication_error") => ModelFailureKind::Authentication,
                    Some("invalid_request_error") => ModelFailureKind::InvalidRequest,
                    _ => ModelFailureKind::Other,
                };
                let mut failure = ModelFailure::new(kind, message.clone());
                failure.provider_code = code.clone();
                failure
            }
            Self::Provider {
                provider,
                code,
                message,
                kind,
                status,
                retry_after,
            } => {
                let mut failure = ModelFailure::new(*kind, message.clone());
                failure.provider = Some(provider.clone());
                failure.provider_code = Some(code.clone());
                failure.status = *status;
                failure.retry_after = *retry_after;
                failure
            }
            Self::Timeout(duration) => ModelFailure::new(
                ModelFailureKind::Timeout,
                format!("request timeout after {duration:?}"),
            ),
            Self::RateLimited { retry_after } => {
                let mut failure = ModelFailure::new(ModelFailureKind::RateLimited, "rate limited");
                failure.retry_after = *retry_after;
                failure
            }
            Self::Authentication(message) => {
                ModelFailure::new(ModelFailureKind::Authentication, message.clone())
            }
            Self::InvalidResponse(message) => {
                ModelFailure::new(ModelFailureKind::InvalidResponse, message.clone())
            }
            Self::NotFound(message) => {
                ModelFailure::new(ModelFailureKind::NotFound, message.clone())
            }
            Self::Cancelled => ModelFailure::new(ModelFailureKind::Cancelled, "request cancelled"),
            Self::Connection(message) | Self::Network(message) => {
                ModelFailure::new(ModelFailureKind::Connection, message.clone())
            }
            Self::ContextLengthExceeded { .. } => {
                ModelFailure::new(ModelFailureKind::InvalidRequest, self.to_string())
            }
            Self::Configuration(message) | Self::NotSupported(message) => {
                ModelFailure::new(ModelFailureKind::Configuration, message.clone())
            }
            Self::IncompleteStream(message) => {
                ModelFailure::new(ModelFailureKind::IncompleteStream, message.clone())
            }
            Self::Serialization(error) => {
                ModelFailure::new(ModelFailureKind::InvalidResponse, error.to_string())
            }
            Self::FallbackExhausted {
                attempts,
                last_error,
            } => {
                let mut failure = last_error.model_failure();
                failure.attempt = Some(attempts.len() as u32);
                return failure;
            }
            Self::RetryExhausted {
                attempts,
                last_error,
                ..
            } => {
                let mut failure = last_error.model_failure();
                failure.attempt = Some(*attempts);
                return failure;
            }
            Self::RetryDeadlineExceeded {
                attempts,
                last_error: Some(last_error),
                ..
            } => {
                let mut failure = last_error.model_failure();
                failure.attempt = Some(*attempts);
                return failure;
            }
            Self::RetryDeadlineExceeded { attempts, .. } => {
                let mut failure = ModelFailure::new(ModelFailureKind::Timeout, self.to_string());
                failure.attempt = Some(*attempts);
                failure
            }
            Self::ContentFiltered(message) => {
                ModelFailure::new(ModelFailureKind::Other, message.clone())
            }
            Self::Other(error) => ModelFailure::new(ModelFailureKind::Other, error.to_string()),
            Self::CoreApi(error) => return error.model_failure(),
            Self::CoreHttp(error) => return error.model_failure(),
        };
        failure.retry_after = failure.retry_after.or_else(|| self.retry_after());
        failure.cause = Some(self.to_string());
        failure
    }
}

impl From<reqwest::Error> for ModelError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            ModelError::Timeout(Duration::from_secs(30)) // Default timeout
        } else if err.is_connect() {
            ModelError::Connection(err.to_string())
        } else if let Some(status) = err.status() {
            ModelError::Http {
                status: status.as_u16(),
                body: err.to_string(),
                headers: HashMap::new(),
            }
        } else {
            ModelError::Other(err.into())
        }
    }
}

/// Result type for model operations.
pub type ModelResult<T> = Result<T, ModelError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_retryable() {
        assert!(ModelError::Timeout(Duration::from_secs(30)).is_retryable());
        assert!(ModelError::rate_limited(None).is_retryable());
        assert!(ModelError::Connection("failed".into()).is_retryable());
        assert!(ModelError::http(500, "Server error").is_retryable());
        assert!(ModelError::http(502, "Bad gateway").is_retryable());

        assert!(!ModelError::http(400, "Bad request").is_retryable());
        assert!(!ModelError::http(401, "Unauthorized").is_retryable());
        assert!(!ModelError::auth("Invalid key").is_retryable());
        assert!(!ModelError::api("Error").is_retryable());
    }

    #[test]
    fn test_retry_after() {
        let err = ModelError::rate_limited(Some(Duration::from_secs(60)));
        assert_eq!(err.retry_after(), Some(Duration::from_secs(60)));

        let err = ModelError::rate_limited(None);
        assert_eq!(err.retry_after(), None);

        let err = ModelError::Timeout(Duration::from_secs(30));
        assert_eq!(err.retry_after(), None);
    }

    #[test]
    fn test_error_display() {
        let err = ModelError::api_with_code("Something went wrong", "INVALID_REQUEST");
        assert!(err.to_string().contains("Something went wrong"));

        let err = ModelError::http(404, "Not found");
        assert!(err.to_string().contains("404"));
    }

    #[test]
    fn canonical_classification_matrix() {
        let cases = [
            (
                ModelError::http(429, "limited"),
                ModelFailureKind::RateLimited,
                true,
            ),
            (
                ModelError::http(500, "server"),
                ModelFailureKind::Server,
                true,
            ),
            (
                ModelError::http(503, "unavailable"),
                ModelFailureKind::Server,
                true,
            ),
            (
                ModelError::http(400, "bad"),
                ModelFailureKind::InvalidRequest,
                false,
            ),
            (
                ModelError::http(401, "auth"),
                ModelFailureKind::Authentication,
                false,
            ),
            (
                ModelError::Timeout(Duration::from_secs(1)),
                ModelFailureKind::Timeout,
                true,
            ),
            (
                ModelError::Connection("reset".into()),
                ModelFailureKind::Connection,
                true,
            ),
            (
                ModelError::invalid_response("bad json"),
                ModelFailureKind::InvalidResponse,
                false,
            ),
            (ModelError::Cancelled, ModelFailureKind::Cancelled, false),
        ];

        for (error, expected_kind, retryable) in cases {
            let failure = error.model_failure();
            assert_eq!(failure.kind, expected_kind);
            assert_eq!(failure.is_retryable(), retryable);
            assert_eq!(error.is_retryable(), retryable);
        }
    }

    #[test]
    fn provider_metadata_and_retry_context_survive_classification() {
        let error = ModelError::RetryExhausted {
            attempts: 3,
            elapsed: Duration::from_secs(2),
            last_error: Box::new(ModelError::provider_with_status(
                "anthropic",
                "rate_limit_error",
                "slow down",
                ModelFailureKind::RateLimited,
                Some(429),
                Some(Duration::from_secs(7)),
            )),
        };

        let failure = error.model_failure();
        assert_eq!(failure.kind, ModelFailureKind::RateLimited);
        assert_eq!(failure.status, Some(429));
        assert_eq!(failure.provider.as_deref(), Some("anthropic"));
        assert_eq!(failure.provider_code.as_deref(), Some("rate_limit_error"));
        assert_eq!(failure.retry_after, Some(Duration::from_secs(7)));
        assert_eq!(failure.attempt, Some(3));
    }

    #[test]
    fn legacy_core_errors_convert_without_losing_source_or_metadata() {
        let mut headers = HashMap::new();
        headers.insert("Retry-After".to_string(), "5".to_string());
        let core_error = ModelApiError::new(429, "limited")
            .with_message("slow down")
            .with_error_code("rate_limit_error")
            .with_headers(headers);
        let error = ModelError::from(core_error);
        let failure = error.model_failure();

        assert_eq!(failure.kind, ModelFailureKind::RateLimited);
        assert_eq!(failure.status, Some(429));
        assert_eq!(failure.provider_code.as_deref(), Some("rate_limit_error"));
        assert_eq!(failure.retry_after, Some(Duration::from_secs(5)));
        assert!(std::error::Error::source(&error).is_some());
    }
}
