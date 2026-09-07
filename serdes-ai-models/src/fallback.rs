//! Fallback model that wraps multiple models and tries them in sequence.
//!
//! This module provides a [`FallbackModel`] that implements resilient model access
//! by trying multiple models in order until one succeeds.
//!
//! # Example
//!
//! ```rust,ignore
//! use serdes_ai_models::fallback::{FallbackModel, RetryOn};
//! use serdes_ai_models::MockModel;
//!
//! let fallback = FallbackModel::new(vec![
//!     Box::new(primary_model),
//!     Box::new(backup_model),
//! ])
//! .with_retry_on(RetryOn::RateLimits);
//!
//! // If primary fails with rate limit, automatically tries backup
//! let response = fallback.request(&messages, &settings, &params).await?;
//! ```

use crate::error::ModelError;
use crate::model::{Model, ModelRequestParameters, StreamedResponse};
use crate::profile::ModelProfile;
use async_trait::async_trait;
use futures::{StreamExt, stream};
use serdes_ai_core::{
    ClassifyModelFailure, ModelFailure, ModelRequest, ModelResponse, ModelSettings,
};
use tracing::{debug, warn};

/// Policy for determining when to retry with the next model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RetryOn {
    /// Retry on any error.
    #[default]
    AnyError,
    /// Only retry on rate limit errors.
    RateLimits,
    /// Only retry on transient errors (timeout, connection, server errors).
    Transient,
}

impl RetryOn {
    /// Check if the given error should trigger a retry.
    #[must_use]
    pub fn should_retry(&self, error: &ModelError) -> bool {
        let failure = error.model_failure();
        match self {
            RetryOn::AnyError => true,
            RetryOn::RateLimits => failure.kind.is_rate_limited(),
            RetryOn::Transient => failure.kind.is_transient(),
        }
    }
}

/// A model that tries multiple models in order until one succeeds.
///
/// This is useful for:
/// - Implementing fallback strategies (e.g., try Claude first, fall back to GPT-4)
/// - Handling rate limits by falling back to alternative models
/// - Testing model behavior with mock fallbacks
///
/// Streaming fallback is allowed only before the caller observes any stream
/// event. Every event, including metadata and terminal events, establishes the
/// selected attempt. Errors after that boundary are propagated from that model
/// and never trigger output replay or concatenation. The wrapper polls at most
/// one event before returning, so buffering and backpressure remain bounded; a
/// losing stream is dropped before the next model is attempted.
pub struct FallbackModel {
    models: Vec<Box<dyn Model>>,
    retry_on: RetryOn,
    profile: ModelProfile,
}

impl std::fmt::Debug for FallbackModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FallbackModel")
            .field("model_count", &self.models.len())
            .field("retry_on", &self.retry_on)
            .finish()
    }
}

impl FallbackModel {
    /// Create a new fallback model with the given models.
    ///
    /// Models are tried in order; the first model that succeeds returns its response.
    ///
    /// # Arguments
    ///
    /// * `models` - List of models to try in order
    ///
    /// # Panics
    ///
    /// Does not panic, but returns an error on requests if the list is empty.
    #[must_use]
    pub fn new(models: Vec<Box<dyn Model>>) -> Self {
        // Use the first model's profile as default, or a default profile if empty
        let profile = models
            .first()
            .map(|m| m.profile().clone())
            .unwrap_or_default();

        Self {
            models,
            retry_on: RetryOn::default(),
            profile,
        }
    }

    /// Set the retry policy.
    ///
    /// # Arguments
    ///
    /// * `retry_on` - When to retry with the next model
    #[must_use]
    pub fn with_retry_on(mut self, retry_on: RetryOn) -> Self {
        self.retry_on = retry_on;
        self
    }

    /// Add another model to the fallback chain.
    ///
    /// # Arguments
    ///
    /// * `model` - Model to add to the end of the chain
    #[must_use]
    pub fn with_model(mut self, model: impl Model + 'static) -> Self {
        self.models.push(Box::new(model));
        self
    }

    /// Set a custom profile for this fallback model.
    ///
    /// By default, uses the first model's profile.
    #[must_use]
    pub fn with_profile(mut self, profile: ModelProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Get the number of models in the fallback chain.
    #[must_use]
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    /// Check if the fallback chain is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    fn contextual_failure(error: &ModelError, model: &dyn Model, attempt: usize) -> ModelFailure {
        let mut failure = error.model_failure();
        if failure.provider.is_none() {
            failure.provider = Some(model.system().to_string());
        }
        failure.model = Some(model.identifier());
        failure.attempt = Some(attempt as u32);
        failure
    }

    fn finish_failure(
        mut attempts: Vec<ModelFailure>,
        error: ModelError,
        model: &dyn Model,
        attempt: usize,
    ) -> ModelError {
        attempts.push(Self::contextual_failure(&error, model, attempt));
        if attempts.len() == 1 {
            error
        } else {
            ModelError::FallbackExhausted {
                attempts,
                last_error: Box::new(error),
            }
        }
    }

    /// Check if we should retry with the next model for the given error.
    fn should_retry(&self, error: &ModelError) -> bool {
        self.retry_on.should_retry(error)
    }
}

#[async_trait]
impl Model for FallbackModel {
    fn name(&self) -> &str {
        "fallback"
    }

    fn system(&self) -> &str {
        "fallback"
    }

    fn identifier(&self) -> String {
        let model_names: Vec<_> = self.models.iter().map(|m| m.identifier()).collect();
        format!("fallback:[{}]", model_names.join(","))
    }

    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    async fn request(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<ModelResponse, ModelError> {
        if self.models.is_empty() {
            return Err(ModelError::configuration("No models in fallback chain"));
        }

        let mut attempts = Vec::new();

        for (i, model) in self.models.iter().enumerate() {
            let attempt = i + 1;
            let is_last = attempt == self.models.len();

            debug!(
                model = %model.identifier(),
                attempt,
                total = self.models.len(),
                "Trying model in fallback chain"
            );

            match model.request(messages, settings, params).await {
                Ok(response) => return Ok(response),
                Err(error) => {
                    warn!(
                        model = %model.identifier(),
                        error = %error,
                        "Model request failed"
                    );

                    if !is_last && self.should_retry(&error) {
                        attempts.push(Self::contextual_failure(&error, model.as_ref(), attempt));
                        continue;
                    }

                    return Err(Self::finish_failure(
                        attempts,
                        error,
                        model.as_ref(),
                        attempt,
                    ));
                }
            }
        }

        Err(ModelError::configuration("No models in fallback chain"))
    }

    async fn request_stream(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<StreamedResponse, ModelError> {
        if self.models.is_empty() {
            return Err(ModelError::configuration("No models in fallback chain"));
        }

        let mut attempts = Vec::new();

        for (i, model) in self.models.iter().enumerate() {
            let attempt = i + 1;
            let is_last = attempt == self.models.len();

            debug!(
                model = %model.identifier(),
                attempt,
                total = self.models.len(),
                "Trying model in fallback chain (streaming)"
            );

            let stream = match model.request_stream(messages, settings, params).await {
                Ok(stream) => stream,
                Err(error) => {
                    if !is_last && self.should_retry(&error) {
                        attempts.push(Self::contextual_failure(&error, model.as_ref(), attempt));
                        continue;
                    }
                    return Err(Self::finish_failure(
                        attempts,
                        error,
                        model.as_ref(),
                        attempt,
                    ));
                }
            };

            let mut stream = stream;
            match stream.next().await {
                Some(Ok(event)) => {
                    return Ok(stream::once(async move { Ok(event) }).chain(stream).boxed());
                }
                Some(Err(error)) => {
                    if !is_last && self.should_retry(&error) {
                        attempts.push(Self::contextual_failure(&error, model.as_ref(), attempt));
                        continue;
                    }
                    return Err(Self::finish_failure(
                        attempts,
                        error,
                        model.as_ref(),
                        attempt,
                    ));
                }
                None => return Ok(stream),
            }
        }

        Err(ModelError::configuration("No models in fallback chain"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderErrorKind;
    use crate::mock::MockModel;
    use serdes_ai_core::messages::{ModelResponseStreamEvent, StreamCompleteEvent, TextPart};
    use serdes_ai_core::{FinishReason, ModelResponsePart};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// A mock model that can fail with configurable errors.
    struct FailingMockModel {
        name: String,
        error: ModelError,
        call_count: Arc<AtomicUsize>,
        profile: ModelProfile,
    }

    impl FailingMockModel {
        fn new(name: impl Into<String>, error: ModelError) -> Self {
            Self {
                name: name.into(),
                error,
                call_count: Arc::new(AtomicUsize::new(0)),
                profile: ModelProfile::default(),
            }
        }

        #[allow(dead_code)]
        fn call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Model for FailingMockModel {
        fn name(&self) -> &str {
            &self.name
        }

        fn system(&self) -> &str {
            "failing-mock"
        }

        fn profile(&self) -> &ModelProfile {
            &self.profile
        }

        async fn request(
            &self,
            _messages: &[ModelRequest],
            _settings: &ModelSettings,
            _params: &ModelRequestParameters,
        ) -> Result<ModelResponse, ModelError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            // Return a new instance of the error type
            match &self.error {
                ModelError::RateLimited { retry_after } => Err(ModelError::RateLimited {
                    retry_after: *retry_after,
                }),
                ModelError::Timeout(d) => Err(ModelError::Timeout(*d)),
                ModelError::Connection(msg) => Err(ModelError::Connection(msg.clone())),
                ModelError::Authentication(msg) => Err(ModelError::Authentication(msg.clone())),
                ModelError::Provider {
                    provider,
                    code,
                    message,
                    kind,
                    status,
                    retry_after,
                } => Err(ModelError::Provider {
                    provider: provider.clone(),
                    code: code.clone(),
                    message: message.clone(),
                    kind: *kind,
                    status: *status,
                    retry_after: *retry_after,
                }),
                ModelError::Http {
                    status,
                    body,
                    headers,
                } => Err(ModelError::Http {
                    status: *status,
                    body: body.clone(),
                    headers: headers.clone(),
                }),
                _ => Err(ModelError::api("Generic error")),
            }
        }

        async fn request_stream(
            &self,
            _messages: &[ModelRequest],
            _settings: &ModelSettings,
            _params: &ModelRequestParameters,
        ) -> Result<StreamedResponse, ModelError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Err(ModelError::api("Stream error"))
        }
    }

    /// A mock model that succeeds and tracks calls.
    struct SucceedingMockModel {
        name: String,
        response_text: String,
        call_count: Arc<AtomicUsize>,
        profile: ModelProfile,
    }

    impl SucceedingMockModel {
        fn new(name: impl Into<String>, response: impl Into<String>) -> Self {
            Self {
                name: name.into(),
                response_text: response.into(),
                call_count: Arc::new(AtomicUsize::new(0)),
                profile: ModelProfile::default(),
            }
        }

        #[allow(dead_code)]
        fn call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Model for SucceedingMockModel {
        fn name(&self) -> &str {
            &self.name
        }

        fn system(&self) -> &str {
            "succeeding-mock"
        }

        fn profile(&self) -> &ModelProfile {
            &self.profile
        }

        async fn request(
            &self,
            _messages: &[ModelRequest],
            _settings: &ModelSettings,
            _params: &ModelRequestParameters,
        ) -> Result<ModelResponse, ModelError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(ModelResponse {
                parts: vec![ModelResponsePart::Text(TextPart::new(&self.response_text))],
                model_name: Some(self.name.clone()),
                timestamp: chrono::Utc::now(),
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                vendor_id: None,
                vendor_details: None,
                kind: "response".to_string(),
            })
        }

        async fn request_stream(
            &self,
            _messages: &[ModelRequest],
            _settings: &ModelSettings,
            _params: &ModelRequestParameters,
        ) -> Result<StreamedResponse, ModelError> {
            Err(ModelError::not_supported("Streaming"))
        }
    }

    #[test]
    fn test_retry_on_should_retry() {
        // AnyError retries on everything
        assert!(RetryOn::AnyError.should_retry(&ModelError::api("test")));
        assert!(RetryOn::AnyError.should_retry(&ModelError::rate_limited(None)));
        assert!(RetryOn::AnyError.should_retry(&ModelError::Timeout(Duration::from_secs(30))));

        // RateLimits only retries on rate limit errors
        assert!(RetryOn::RateLimits.should_retry(&ModelError::rate_limited(None)));
        assert!(!RetryOn::RateLimits.should_retry(&ModelError::api("test")));
        assert!(!RetryOn::RateLimits.should_retry(&ModelError::Timeout(Duration::from_secs(30))));
        let provider_rate_limit = ModelError::provider(
            "anthropic",
            "rate_limit_error",
            "slow down",
            ProviderErrorKind::RateLimited,
            None,
        );
        assert!(RetryOn::RateLimits.should_retry(&provider_rate_limit));

        // Transient retries on timeout, connection, network, and 5xx errors
        assert!(RetryOn::Transient.should_retry(&ModelError::Timeout(Duration::from_secs(30))));
        assert!(RetryOn::Transient.should_retry(&ModelError::Connection("failed".into())));
        assert!(RetryOn::Transient.should_retry(&ModelError::Network("failed".into())));
        assert!(RetryOn::Transient.should_retry(&ModelError::http(500, "Server error")));
        assert!(RetryOn::Transient.should_retry(&ModelError::http(502, "Bad gateway")));
        assert!(!RetryOn::Transient.should_retry(&ModelError::http(400, "Bad request")));
        assert!(!RetryOn::Transient.should_retry(&ModelError::api("test")));
        let provider_overload = ModelError::provider(
            "anthropic",
            "overloaded_error",
            "busy",
            ProviderErrorKind::Overloaded,
            None,
        );
        assert!(RetryOn::Transient.should_retry(&provider_overload));
        assert!(!RetryOn::Transient.should_retry(&ModelError::rate_limited(None)));
    }

    #[test]
    fn test_fallback_model_new() {
        let model1 = MockModel::new("model1");
        let model2 = MockModel::new("model2");

        let fallback = FallbackModel::new(vec![Box::new(model1), Box::new(model2)]);

        assert_eq!(fallback.name(), "fallback");
        assert_eq!(fallback.system(), "fallback");
        assert_eq!(fallback.model_count(), 2);
        assert!(!fallback.is_empty());
    }

    #[test]
    fn test_fallback_model_with_model() {
        let model1 = MockModel::new("model1");
        let model2 = MockModel::new("model2");

        let fallback = FallbackModel::new(vec![Box::new(model1)]).with_model(model2);

        assert_eq!(fallback.model_count(), 2);
    }

    #[test]
    fn test_fallback_model_identifier() {
        let model1 = MockModel::new("model1");
        let model2 = MockModel::new("model2");

        let fallback = FallbackModel::new(vec![Box::new(model1), Box::new(model2)]);

        assert_eq!(fallback.identifier(), "fallback:[mock:model1,mock:model2]");
    }

    #[test]
    fn test_fallback_empty() {
        let fallback: FallbackModel = FallbackModel::new(vec![]);
        assert!(fallback.is_empty());
        assert_eq!(fallback.model_count(), 0);
    }

    #[tokio::test]
    async fn test_fallback_first_model_succeeds() {
        let model1 = SucceedingMockModel::new("model1", "response1");
        let model2 = SucceedingMockModel::new("model2", "response2");

        let call_count1 = model1.call_count.clone();
        let call_count2 = model2.call_count.clone();

        let fallback = FallbackModel::new(vec![Box::new(model1), Box::new(model2)]);

        let messages = vec![ModelRequest::new()];
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let response = fallback
            .request(&messages, &settings, &params)
            .await
            .unwrap();

        // First model should succeed, second should not be called
        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 0);

        // Should get first model's response
        if let ModelResponsePart::Text(text) = &response.parts[0] {
            assert_eq!(text.content, "response1");
        } else {
            panic!("Expected text response");
        }
    }

    #[tokio::test]
    async fn test_fallback_first_fails_second_succeeds() {
        let model1 = FailingMockModel::new("model1", ModelError::rate_limited(None));
        let model2 = SucceedingMockModel::new("model2", "response2");

        let call_count1 = model1.call_count.clone();
        let call_count2 = model2.call_count.clone();

        let fallback = FallbackModel::new(vec![Box::new(model1), Box::new(model2)]);

        let messages = vec![ModelRequest::new()];
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let response = fallback
            .request(&messages, &settings, &params)
            .await
            .unwrap();

        // Both models should be called
        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 1);

        // Should get second model's response
        if let ModelResponsePart::Text(text) = &response.parts[0] {
            assert_eq!(text.content, "response2");
        } else {
            panic!("Expected text response");
        }
    }

    #[tokio::test]
    async fn test_fallback_all_models_fail() {
        let model1 = FailingMockModel::new("model1", ModelError::rate_limited(None));
        let model2 = FailingMockModel::new("model2", ModelError::rate_limited(None));

        let call_count1 = model1.call_count.clone();
        let call_count2 = model2.call_count.clone();

        let fallback = FallbackModel::new(vec![Box::new(model1), Box::new(model2)]);

        let messages = vec![ModelRequest::new()];
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let result = fallback.request(&messages, &settings, &params).await;

        // Both models should be called
        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 1);

        // Preserve both failed model attempts and the final concrete source.
        match result.unwrap_err() {
            ModelError::FallbackExhausted { attempts, .. } => {
                assert_eq!(attempts.len(), 2);
                assert_eq!(attempts[0].model.as_deref(), Some("failing-mock:model1"));
                assert_eq!(attempts[1].model.as_deref(), Some("failing-mock:model2"));
            }
            other => panic!("expected fallback context, got {other}"),
        }
    }

    #[tokio::test]
    async fn test_fallback_empty_chain_error() {
        let fallback: FallbackModel = FallbackModel::new(vec![]);

        let messages = vec![ModelRequest::new()];
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let result = fallback.request(&messages, &settings, &params).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ModelError::Configuration(_)));
        assert!(err.to_string().contains("No models in fallback chain"));
    }

    #[tokio::test]
    async fn test_fallback_retry_on_rate_limits_only() {
        // First model fails with auth error (not retryable with RateLimits policy)
        let model1 = FailingMockModel::new("model1", ModelError::auth("Invalid key"));
        let model2 = SucceedingMockModel::new("model2", "response2");

        let call_count1 = model1.call_count.clone();
        let call_count2 = model2.call_count.clone();

        let fallback = FallbackModel::new(vec![Box::new(model1), Box::new(model2)])
            .with_retry_on(RetryOn::RateLimits);

        let messages = vec![ModelRequest::new()];
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let result = fallback.request(&messages, &settings, &params).await;

        // First model should be called, second should NOT (auth error is not retryable)
        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 0);

        // Should return auth error
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ModelError::Authentication(_)));
    }

    #[tokio::test]
    async fn test_fallback_retry_on_rate_limits_succeeds() {
        // First model fails with rate limit (retryable)
        let model1 = FailingMockModel::new("model1", ModelError::rate_limited(None));
        let model2 = SucceedingMockModel::new("model2", "response2");

        let call_count1 = model1.call_count.clone();
        let call_count2 = model2.call_count.clone();

        let fallback = FallbackModel::new(vec![Box::new(model1), Box::new(model2)])
            .with_retry_on(RetryOn::RateLimits);

        let messages = vec![ModelRequest::new()];
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let response = fallback
            .request(&messages, &settings, &params)
            .await
            .unwrap();

        // Both models should be called
        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 1);

        if let ModelResponsePart::Text(text) = &response.parts[0] {
            assert_eq!(text.content, "response2");
        } else {
            panic!("Expected text response");
        }
    }

    #[tokio::test]
    async fn test_fallback_retry_on_transient() {
        // First model fails with timeout (transient)
        let model1 = FailingMockModel::new("model1", ModelError::Timeout(Duration::from_secs(30)));
        let model2 = SucceedingMockModel::new("model2", "response2");

        let call_count1 = model1.call_count.clone();
        let call_count2 = model2.call_count.clone();

        let fallback = FallbackModel::new(vec![Box::new(model1), Box::new(model2)])
            .with_retry_on(RetryOn::Transient);

        let messages = vec![ModelRequest::new()];
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let response = fallback
            .request(&messages, &settings, &params)
            .await
            .unwrap();

        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 1);

        if let ModelResponsePart::Text(text) = &response.parts[0] {
            assert_eq!(text.content, "response2");
        } else {
            panic!("Expected text response");
        }
    }

    #[tokio::test]
    async fn test_fallback_transient_does_not_retry_on_rate_limit() {
        // First model fails with rate limit (NOT transient)
        let model1 = FailingMockModel::new("model1", ModelError::rate_limited(None));
        let model2 = SucceedingMockModel::new("model2", "response2");

        let call_count1 = model1.call_count.clone();
        let call_count2 = model2.call_count.clone();

        let fallback = FallbackModel::new(vec![Box::new(model1), Box::new(model2)])
            .with_retry_on(RetryOn::Transient);

        let messages = vec![ModelRequest::new()];
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let result = fallback.request(&messages, &settings, &params).await;

        // Only first model should be called
        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 0);

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ModelError::RateLimited { .. }
        ));
    }

    #[tokio::test]
    async fn test_fallback_three_models() {
        let model1 = FailingMockModel::new("model1", ModelError::rate_limited(None));
        let model2 = FailingMockModel::new("model2", ModelError::Timeout(Duration::from_secs(30)));
        let model3 = SucceedingMockModel::new("model3", "response3");

        let call_count1 = model1.call_count.clone();
        let call_count2 = model2.call_count.clone();
        let call_count3 = model3.call_count.clone();

        let fallback =
            FallbackModel::new(vec![Box::new(model1), Box::new(model2), Box::new(model3)]);

        let messages = vec![ModelRequest::new()];
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let response = fallback
            .request(&messages, &settings, &params)
            .await
            .unwrap();

        // All three models should be called
        assert_eq!(call_count1.load(Ordering::SeqCst), 1);
        assert_eq!(call_count2.load(Ordering::SeqCst), 1);
        assert_eq!(call_count3.load(Ordering::SeqCst), 1);

        if let ModelResponsePart::Text(text) = &response.parts[0] {
            assert_eq!(text.content, "response3");
        } else {
            panic!("Expected text response");
        }
    }

    #[tokio::test]
    async fn test_fallback_rate_limit_policy_accepts_provider_classification() {
        let primary = FailingMockModel::new(
            "primary",
            ModelError::provider(
                "anthropic",
                "rate_limit_error",
                "slow down",
                ProviderErrorKind::RateLimited,
                Some(Duration::from_secs(3)),
            ),
        );
        let backup = SucceedingMockModel::new("backup", "ok");
        let backup_calls = backup.call_count.clone();
        let fallback = FallbackModel::new(vec![Box::new(primary), Box::new(backup)])
            .with_retry_on(RetryOn::RateLimits);

        let result = fallback
            .request(
                &[ModelRequest::new()],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(backup_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_fallback_transient_policy_accepts_provider_overload() {
        let primary = FailingMockModel::new(
            "primary",
            ModelError::provider(
                "anthropic",
                "overloaded_error",
                "busy",
                ProviderErrorKind::Overloaded,
                None,
            ),
        );
        let backup = SucceedingMockModel::new("backup", "ok");
        let backup_calls = backup.call_count.clone();
        let fallback = FallbackModel::new(vec![Box::new(primary), Box::new(backup)])
            .with_retry_on(RetryOn::Transient);

        let result = fallback
            .request(
                &[ModelRequest::new()],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(backup_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_default_retry_on() {
        let retry_on = RetryOn::default();
        assert_eq!(retry_on, RetryOn::AnyError);
    }

    #[tokio::test]
    async fn test_fallback_stream_empty_chain() {
        let fallback: FallbackModel = FallbackModel::new(vec![]);

        let messages = vec![ModelRequest::new()];
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let result = fallback.request_stream(&messages, &settings, &params).await;

        assert!(result.is_err());
        match result {
            Err(ModelError::Configuration(msg)) => {
                assert!(msg.contains("No models in fallback chain"));
            }
            _ => panic!("Expected Configuration error"),
        }
    }

    struct StreamingMockModel {
        name: String,
        events: std::sync::Mutex<Option<Vec<Result<ModelResponseStreamEvent, ModelError>>>>,
        calls: Arc<AtomicUsize>,
        profile: ModelProfile,
    }

    impl StreamingMockModel {
        fn new(
            name: impl Into<String>,
            events: Vec<Result<ModelResponseStreamEvent, ModelError>>,
        ) -> Self {
            Self {
                name: name.into(),
                events: std::sync::Mutex::new(Some(events)),
                calls: Arc::new(AtomicUsize::new(0)),
                profile: ModelProfile::default(),
            }
        }
    }

    #[async_trait]
    impl Model for StreamingMockModel {
        fn name(&self) -> &str {
            &self.name
        }

        fn system(&self) -> &str {
            "streaming-mock"
        }

        fn profile(&self) -> &ModelProfile {
            &self.profile
        }

        async fn request(
            &self,
            _messages: &[ModelRequest],
            _settings: &ModelSettings,
            _params: &ModelRequestParameters,
        ) -> Result<ModelResponse, ModelError> {
            Err(ModelError::not_supported("non-streaming request"))
        }

        async fn request_stream(
            &self,
            _messages: &[ModelRequest],
            _settings: &ModelSettings,
            _params: &ModelRequestParameters,
        ) -> Result<StreamedResponse, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let events = self.events.lock().unwrap().take().unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn stream_falls_back_after_retryable_error_before_first_event() {
        let primary =
            StreamingMockModel::new("primary", vec![Err(ModelError::Connection("reset".into()))]);
        let backup = StreamingMockModel::new(
            "backup",
            vec![Ok(ModelResponseStreamEvent::part_start(
                0,
                ModelResponsePart::Text(TextPart::new("backup")),
            ))],
        );
        let backup_calls = Arc::clone(&backup.calls);
        let fallback = FallbackModel::new(vec![Box::new(primary), Box::new(backup)])
            .with_retry_on(RetryOn::Transient);

        let mut stream = fallback
            .request_stream(
                &[ModelRequest::new()],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            )
            .await
            .unwrap();

        assert!(matches!(
            stream.next().await,
            Some(Ok(ModelResponseStreamEvent::PartStart(_)))
        ));
        assert_eq!(backup_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_does_not_fallback_after_any_event_is_exposed() {
        let primary = StreamingMockModel::new(
            "primary",
            vec![
                Ok(ModelResponseStreamEvent::part_start(
                    0,
                    ModelResponsePart::Text(TextPart::new("primary")),
                )),
                Err(ModelError::Connection("late reset".into())),
            ],
        );
        let backup = StreamingMockModel::new(
            "backup",
            vec![Ok(ModelResponseStreamEvent::part_start(
                0,
                ModelResponsePart::Text(TextPart::new("backup")),
            ))],
        );
        let backup_calls = Arc::clone(&backup.calls);
        let fallback = FallbackModel::new(vec![Box::new(primary), Box::new(backup)])
            .with_retry_on(RetryOn::Transient);

        let mut stream = fallback
            .request_stream(
                &[ModelRequest::new()],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            )
            .await
            .unwrap();

        assert!(stream.next().await.unwrap().is_ok());
        assert!(matches!(
            stream.next().await,
            Some(Err(ModelError::Connection(message))) if message == "late reset"
        ));
        assert_eq!(backup_calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn terminal_metadata_event_also_prevents_fallback() {
        let primary = StreamingMockModel::new(
            "primary",
            vec![
                Ok(ModelResponseStreamEvent::StreamComplete(
                    StreamCompleteEvent::new(FinishReason::Stop),
                )),
                Err(ModelError::Connection("after metadata".into())),
            ],
        );
        let backup = StreamingMockModel::new("backup", vec![]);
        let backup_calls = Arc::clone(&backup.calls);
        let fallback = FallbackModel::new(vec![Box::new(primary), Box::new(backup)])
            .with_retry_on(RetryOn::Transient);

        let mut stream = fallback
            .request_stream(
                &[ModelRequest::new()],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            )
            .await
            .unwrap();

        assert!(matches!(
            stream.next().await,
            Some(Ok(ModelResponseStreamEvent::StreamComplete(_)))
        ));
        assert!(matches!(
            stream.next().await,
            Some(Err(ModelError::Connection(message))) if message == "after metadata"
        ));
        assert_eq!(backup_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn non_retryable_pre_output_error_does_not_try_backup() {
        let primary = StreamingMockModel::new(
            "primary",
            vec![Err(ModelError::Authentication("bad key".into()))],
        );
        let backup = StreamingMockModel::new("backup", vec![]);
        let backup_calls = Arc::clone(&backup.calls);
        let fallback = FallbackModel::new(vec![Box::new(primary), Box::new(backup)])
            .with_retry_on(RetryOn::Transient);

        let result = fallback
            .request_stream(
                &[ModelRequest::new()],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            )
            .await;

        assert!(matches!(result, Err(ModelError::Authentication(_))));
        assert_eq!(backup_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn all_pre_output_failures_preserve_model_attempt_context() {
        let primary = StreamingMockModel::new(
            "primary",
            vec![Err(ModelError::Connection("primary reset".into()))],
        );
        let backup = StreamingMockModel::new(
            "backup",
            vec![Err(ModelError::Timeout(Duration::from_secs(1)))],
        );
        let fallback = FallbackModel::new(vec![Box::new(primary), Box::new(backup)])
            .with_retry_on(RetryOn::Transient);

        let error = match fallback
            .request_stream(
                &[ModelRequest::new()],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            )
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("all candidates should fail"),
        };

        match error {
            ModelError::FallbackExhausted { attempts, .. } => {
                assert_eq!(attempts.len(), 2);
                assert_eq!(attempts[0].model.as_deref(), Some("streaming-mock:primary"));
                assert_eq!(attempts[0].attempt, Some(1));
                assert_eq!(attempts[1].model.as_deref(), Some("streaming-mock:backup"));
                assert_eq!(attempts[1].attempt, Some(2));
            }
            other => panic!("expected fallback context, got {other}"),
        }
    }

    struct PendingStreamingModel {
        calls: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
        profile: ModelProfile,
    }

    struct StreamDropGuard(Arc<AtomicUsize>);

    impl Drop for StreamDropGuard {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl Model for PendingStreamingModel {
        fn name(&self) -> &str {
            "pending"
        }

        fn system(&self) -> &str {
            "streaming-mock"
        }

        fn profile(&self) -> &ModelProfile {
            &self.profile
        }

        async fn request(
            &self,
            _messages: &[ModelRequest],
            _settings: &ModelSettings,
            _params: &ModelRequestParameters,
        ) -> Result<ModelResponse, ModelError> {
            unreachable!()
        }

        async fn request_stream(
            &self,
            _messages: &[ModelRequest],
            _settings: &ModelSettings,
            _params: &ModelRequestParameters,
        ) -> Result<StreamedResponse, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let guard = StreamDropGuard(Arc::clone(&self.drops));
            Ok(Box::pin(futures::stream::unfold(
                guard,
                |guard| async move {
                    futures::future::pending::<()>().await;
                    Some((Err(ModelError::Connection("unreachable".into())), guard))
                },
            )))
        }
    }

    #[tokio::test]
    async fn cancelling_initial_poll_closes_stream_without_trying_backup() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let primary_drops = Arc::new(AtomicUsize::new(0));
        let primary = PendingStreamingModel {
            calls: Arc::clone(&primary_calls),
            drops: Arc::clone(&primary_drops),
            profile: ModelProfile::default(),
        };
        let backup = StreamingMockModel::new("backup", vec![]);
        let backup_calls = Arc::clone(&backup.calls);
        let fallback = FallbackModel::new(vec![Box::new(primary), Box::new(backup)]);

        let result = tokio::time::timeout(
            Duration::from_millis(10),
            fallback.request_stream(
                &[ModelRequest::new()],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            ),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(primary_calls.load(Ordering::SeqCst), 1);
        assert_eq!(primary_drops.load(Ordering::SeqCst), 1);
        assert_eq!(backup_calls.load(Ordering::SeqCst), 0);
    }
}
