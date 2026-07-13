//! Anthropic provider error classification.

use crate::error::{ModelError, ProviderErrorKind};
use std::time::Duration;

/// Convert an Anthropic provider error into the canonical model error form.
pub(super) fn map_anthropic_error(
    code: impl Into<String>,
    message: impl Into<String>,
    retry_after: Option<Duration>,
    http_status: Option<u16>,
) -> ModelError {
    let code = code.into();
    let kind = match code.as_str() {
        "rate_limit_error" => ProviderErrorKind::RateLimited,
        "overloaded_error" => ProviderErrorKind::Overloaded,
        "api_error" => ProviderErrorKind::Server,
        "authentication_error" => ProviderErrorKind::Authentication,
        "invalid_request_error" => ProviderErrorKind::InvalidRequest,
        "not_found_error" => ProviderErrorKind::NotFound,
        "permission_error" => ProviderErrorKind::PermissionDenied,
        "billing_error" => ProviderErrorKind::Billing,
        "request_too_large" => ProviderErrorKind::RequestTooLarge,
        _ if http_status == Some(429) => ProviderErrorKind::RateLimited,
        _ if http_status.is_some_and(|status| status >= 500) => ProviderErrorKind::Server,
        _ => ProviderErrorKind::Other,
    };

    ModelError::provider("anthropic", code, message, kind, retry_after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_retryable_errors() {
        let rate_limit = map_anthropic_error(
            "rate_limit_error",
            "slow down",
            Some(Duration::from_secs(12)),
            Some(429),
        );
        assert!(rate_limit.is_rate_limited());
        assert!(rate_limit.is_retryable());
        assert_eq!(rate_limit.retry_after(), Some(Duration::from_secs(12)));

        let overloaded = map_anthropic_error("overloaded_error", "try again", None, Some(529));
        assert!(overloaded.is_transient());
        assert!(overloaded.is_retryable());
    }

    #[test]
    fn classifies_permanent_errors() {
        for code in ["authentication_error", "invalid_request_error"] {
            let error = map_anthropic_error(code, "permanent", None, Some(400));
            assert!(!error.is_retryable(), "{code} must not be retryable");
        }
    }

    #[test]
    fn preserves_provider_metadata() {
        let error = map_anthropic_error("overloaded_error", "capacity exhausted", None, None);

        match error {
            ModelError::Provider {
                provider,
                code,
                message,
                kind,
                retry_after,
            } => {
                assert_eq!(provider, "anthropic");
                assert_eq!(code, "overloaded_error");
                assert_eq!(message, "capacity exhausted");
                assert_eq!(kind, ProviderErrorKind::Overloaded);
                assert_eq!(retry_after, None);
            }
            other => panic!("expected provider error, got {other:?}"),
        }
    }

    #[test]
    fn transport_does_not_change_known_provider_semantics() {
        for code in [
            "rate_limit_error",
            "overloaded_error",
            "authentication_error",
            "invalid_request_error",
        ] {
            let http = map_anthropic_error(code, "message", None, Some(500));
            let sse = map_anthropic_error(code, "message", None, None);
            assert_eq!(http.is_retryable(), sse.is_retryable(), "{code}");
            assert_eq!(http.is_rate_limited(), sse.is_rate_limited(), "{code}");
            assert_eq!(http.is_transient(), sse.is_transient(), "{code}");
        }
    }
}
