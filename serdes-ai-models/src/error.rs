//! Model-related error types.

use std::collections::HashMap;
use std::time::Duration;
use thiserror::Error;

/// Semantic classification for a provider-reported API error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorKind {
    /// The provider rate-limited the request.
    RateLimited,
    /// The provider is temporarily overloaded.
    Overloaded,
    /// The provider reported a transient server failure.
    Server,
    /// Authentication failed.
    Authentication,
    /// The request was invalid.
    InvalidRequest,
    /// The requested resource was not found.
    NotFound,
    /// The caller lacks permission for the request.
    PermissionDenied,
    /// The account cannot make the request for billing reasons.
    Billing,
    /// The request exceeded the provider's size limit.
    RequestTooLarge,
    /// An unclassified provider error.
    Other,
}

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

    /// Network error.
    #[error("Network error: {0}")]
    Network(String),

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

    /// Other error.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ModelError {
    /// Check if this error is retryable.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.is_rate_limited() || self.is_transient()
    }

    /// Check if this error represents rate limiting.
    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        match self {
            ModelError::RateLimited { .. } => true,
            ModelError::Http { status: 429, .. } => true,
            ModelError::Provider {
                kind: ProviderErrorKind::RateLimited,
                ..
            } => true,
            ModelError::RetryExhausted { last_error, .. } => last_error.is_rate_limited(),
            ModelError::RetryDeadlineExceeded {
                last_error: Some(last_error),
                ..
            } => last_error.is_rate_limited(),
            _ => false,
        }
    }

    /// Check if this error is transient, excluding rate limits.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            ModelError::Timeout(_) | ModelError::Connection(_) | ModelError::Network(_) => true,
            ModelError::Http { status, .. } => (500..=599).contains(status),
            ModelError::Provider { kind, .. } => {
                matches!(
                    kind,
                    ProviderErrorKind::Overloaded | ProviderErrorKind::Server
                )
            }
            ModelError::RetryExhausted { last_error, .. } => last_error.is_transient(),
            ModelError::RetryDeadlineExceeded {
                last_error: Some(last_error),
                ..
            } => last_error.is_transient(),
            _ => false,
        }
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
            ModelError::RetryExhausted { last_error, .. } => last_error.retry_after(),
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
}
