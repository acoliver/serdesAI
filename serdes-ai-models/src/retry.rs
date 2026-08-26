//! Provider-neutral model retry orchestration.

use crate::{
    BoxedModel, Model, ModelError, ModelProfile, ModelRequestParameters, StreamedResponse,
};
use async_trait::async_trait;
use futures::{StreamExt, stream};
use serdes_ai_core::{ModelRequest, ModelResponse, ModelSettings};
use serdes_ai_retries::{RetryDecision, RetryFailure, RetryPolicy, with_retry_policy};
use std::sync::Arc;

/// A model decorator that retries safe failures against the same model.
pub struct RetryingModel {
    inner: BoxedModel,
    policy: RetryPolicy,
}

impl std::fmt::Debug for RetryingModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetryingModel")
            .field("model", &self.inner.identifier())
            .field("policy", &self.policy)
            .finish()
    }
}

impl RetryingModel {
    /// Wrap a concrete model with a retry policy.
    pub fn new<M: Model + 'static>(inner: M, policy: RetryPolicy) -> Self {
        Self::from_arc(Arc::new(inner), policy)
    }

    /// Wrap a dynamically dispatched model with a retry policy.
    pub fn from_arc(inner: BoxedModel, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }

    /// Return the configured retry policy.
    #[must_use]
    pub fn policy(&self) -> &RetryPolicy {
        &self.policy
    }

    /// Return the wrapped model.
    #[must_use]
    pub fn inner(&self) -> &BoxedModel {
        &self.inner
    }
}

/// Extension trait for adding same-model retries to a concrete model.
pub trait ModelRetryExt: Model + Sized + 'static {
    /// Wrap this model with the supplied retry policy.
    fn with_retries(self, policy: RetryPolicy) -> RetryingModel {
        RetryingModel::new(self, policy)
    }
}

impl<M: Model + Sized + 'static> ModelRetryExt for M {}

fn classify(error: &ModelError) -> RetryDecision {
    if error.is_retryable() {
        RetryDecision::Retry {
            retry_after: error.retry_after(),
        }
    } else {
        RetryDecision::DoNotRetry
    }
}

fn adapt_failure(failure: RetryFailure<ModelError>) -> ModelError {
    match failure {
        RetryFailure::Permanent { error, .. } => error,
        RetryFailure::Exhausted {
            error, attempts: 1, ..
        } => error,
        RetryFailure::Exhausted {
            error,
            attempts,
            elapsed,
        } => ModelError::RetryExhausted {
            attempts,
            elapsed,
            last_error: Box::new(error),
        },
        RetryFailure::DeadlineExceeded {
            last_error,
            attempts,
            elapsed,
        } => ModelError::RetryDeadlineExceeded {
            attempts,
            elapsed,
            last_error: last_error.map(Box::new),
        },
    }
}

#[async_trait]
impl Model for RetryingModel {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn system(&self) -> &str {
        self.inner.system()
    }

    fn identifier(&self) -> String {
        self.inner.identifier()
    }

    async fn request(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<ModelResponse, ModelError> {
        with_retry_policy(
            &self.policy,
            || self.inner.request(messages, settings, params),
            classify,
        )
        .await
        .map_err(adapt_failure)
    }

    async fn request_stream(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<StreamedResponse, ModelError> {
        with_retry_policy(
            &self.policy,
            || async {
                let mut response = self
                    .inner
                    .request_stream(messages, settings, params)
                    .await?;

                match response.next().await {
                    Some(Ok(event)) => Ok(Box::pin(
                        stream::once(async move { Ok(event) }).chain(response),
                    ) as StreamedResponse),
                    Some(Err(error)) => Err(error),
                    None => Ok(Box::pin(stream::empty()) as StreamedResponse),
                }
            },
            classify,
        )
        .await
        .map_err(adapt_failure)
    }

    fn profile(&self) -> &ModelProfile {
        self.inner.profile()
    }

    async fn count_tokens(&self, messages: &[ModelRequest]) -> Result<u64, ModelError> {
        // Token counting is intentionally not retried: providers currently implement it
        // locally, while this policy governs model request transport operations.
        self.inner.count_tokens(messages).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEFAULT_PROFILE, FunctionModel, ProviderErrorKind};
    use futures::{StreamExt, stream};
    use serdes_ai_core::{ModelResponsePart, TextPart};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    #[derive(Debug)]
    struct ScriptedRequestModel {
        calls: Arc<AtomicU32>,
        mode: RequestMode,
    }

    #[derive(Debug, Clone, Copy)]
    enum RequestMode {
        TransientThenSuccess,
        Permanent,
        ProviderOverload,
    }

    #[async_trait]
    impl Model for ScriptedRequestModel {
        fn name(&self) -> &str {
            "scripted"
        }

        fn system(&self) -> &str {
            "test"
        }

        async fn request(
            &self,
            _messages: &[ModelRequest],
            _settings: &ModelSettings,
            _params: &ModelRequestParameters,
        ) -> Result<ModelResponse, ModelError> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                RequestMode::TransientThenSuccess if attempt == 0 => {
                    Err(ModelError::Connection("reset".into()))
                }
                RequestMode::TransientThenSuccess => Ok(ModelResponse::text("ok")),
                RequestMode::Permanent => Err(ModelError::auth("bad key")),
                RequestMode::ProviderOverload => Err(ModelError::provider(
                    "anthropic",
                    "overloaded_error",
                    "busy",
                    ProviderErrorKind::Overloaded,
                    Some(Duration::from_secs(2)),
                )),
            }
        }

        async fn request_stream(
            &self,
            _messages: &[ModelRequest],
            _settings: &ModelSettings,
            _params: &ModelRequestParameters,
        ) -> Result<StreamedResponse, ModelError> {
            Err(ModelError::not_supported("streaming"))
        }

        fn profile(&self) -> &ModelProfile {
            &DEFAULT_PROFILE
        }
    }

    fn request_args() -> (Vec<ModelRequest>, ModelSettings, ModelRequestParameters) {
        (
            vec![ModelRequest::new()],
            ModelSettings::default(),
            ModelRequestParameters::new(),
        )
    }

    #[tokio::test]
    async fn retries_transient_failure_then_succeeds() {
        let calls = Arc::new(AtomicU32::new(0));
        let model = ScriptedRequestModel {
            calls: calls.clone(),
            mode: RequestMode::TransientThenSuccess,
        };
        let model = RetryingModel::new(
            model,
            RetryPolicy::for_model_requests()
                .max_attempts(2)
                .wait(serdes_ai_retries::WaitStrategy::None)
                .total_timeout(None),
        );
        let (messages, settings, params) = request_args();

        let response = model.request(&messages, &settings, &params).await.unwrap();

        assert_eq!(response.parts.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn permanent_error_is_not_retried() {
        let calls = Arc::new(AtomicU32::new(0));
        let model = ScriptedRequestModel {
            calls: calls.clone(),
            mode: RequestMode::Permanent,
        };
        let model = RetryingModel::new(
            model,
            RetryPolicy::for_model_requests()
                .max_attempts(3)
                .wait(serdes_ai_retries::WaitStrategy::None),
        );
        let (messages, settings, params) = request_args();

        let error = model
            .request(&messages, &settings, &params)
            .await
            .unwrap_err();

        assert!(matches!(error, ModelError::Authentication(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exhaustion_preserves_provider_metadata() {
        let model = ScriptedRequestModel {
            calls: Arc::new(AtomicU32::new(0)),
            mode: RequestMode::ProviderOverload,
        };
        let model = RetryingModel::new(
            model,
            RetryPolicy::for_model_requests()
                .max_attempts(2)
                .wait(serdes_ai_retries::WaitStrategy::None)
                .total_timeout(None),
        );
        let (messages, settings, params) = request_args();

        let error = model
            .request(&messages, &settings, &params)
            .await
            .unwrap_err();

        assert!(error.is_transient());
        assert_eq!(error.retry_after(), Some(Duration::from_secs(2)));
        match error {
            ModelError::RetryExhausted {
                attempts,
                last_error,
                ..
            } => {
                assert_eq!(attempts, 2);
                assert!(matches!(
                    *last_error,
                    ModelError::Provider {
                        code,
                        message,
                        ..
                    } if code == "overloaded_error" && message == "busy"
                ));
            }
            other => panic!("expected retry exhaustion, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn retries_first_stream_error_before_visible_output() {
        let calls = Arc::new(AtomicU32::new(0));
        let model = {
            let calls = calls.clone();
            FunctionModel::with_stream(move |_, _| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Box::pin(stream::iter(vec![Err(ModelError::Connection(
                        "reset".into(),
                    ))]))
                } else {
                    Box::pin(stream::iter(vec![Ok(
                        serdes_ai_core::ModelResponseStreamEvent::part_start(
                            0,
                            ModelResponsePart::Text(TextPart::new("ok")),
                        ),
                    )]))
                }
            })
        };
        let model = RetryingModel::new(
            model,
            RetryPolicy::for_model_requests()
                .max_attempts(2)
                .wait(serdes_ai_retries::WaitStrategy::None)
                .total_timeout(None),
        );
        let (messages, settings, params) = request_args();

        let events: Vec<_> = model
            .request_stream(&messages, &settings, &params)
            .await
            .unwrap()
            .collect()
            .await;

        assert_eq!(events.len(), 1);
        assert!(events[0].is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_retry_after_delays_before_visible_output() {
        let calls = Arc::new(AtomicU32::new(0));
        let model = {
            let calls = calls.clone();
            FunctionModel::with_stream(move |_, _| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Box::pin(stream::iter(vec![Err(ModelError::provider(
                        "anthropic",
                        "overloaded_error",
                        "busy",
                        ProviderErrorKind::Overloaded,
                        Some(Duration::from_secs(2)),
                    ))]))
                } else {
                    Box::pin(stream::iter(vec![Ok(
                        serdes_ai_core::ModelResponseStreamEvent::part_start(
                            0,
                            ModelResponsePart::Text(TextPart::new("ok")),
                        ),
                    )]))
                }
            })
        };
        let model = RetryingModel::new(
            model,
            RetryPolicy::for_model_requests()
                .max_attempts(2)
                .wait(serdes_ai_retries::WaitStrategy::RetryAfter {
                    fallback: Box::new(serdes_ai_retries::WaitStrategy::None),
                    max_wait: Duration::from_secs(10),
                })
                .total_timeout(Some(Duration::from_secs(5))),
        );
        let (messages, settings, params) = request_args();

        let started = tokio::time::Instant::now();
        let events: Vec<_> = model
            .request_stream(&messages, &settings, &params)
            .await
            .unwrap()
            .collect()
            .await;

        assert_eq!(events.len(), 1);
        assert!(events[0].is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(started.elapsed(), Duration::from_secs(2));
    }

    #[tokio::test(start_paused = true)]
    async fn stream_acquisition_respects_total_deadline() {
        let calls = Arc::new(AtomicU32::new(0));
        let model = {
            let calls = calls.clone();
            FunctionModel::with_stream(move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(stream::once(async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Err(ModelError::Connection("late reset".into()))
                }))
            })
        };
        let model = RetryingModel::new(
            model,
            RetryPolicy::for_model_requests()
                .max_attempts(3)
                .wait(serdes_ai_retries::WaitStrategy::None)
                .total_timeout(Some(Duration::from_secs(5))),
        );
        let (messages, settings, params) = request_args();

        let error = match model.request_stream(&messages, &settings, &params).await {
            Ok(_) => panic!("expected stream deadline error"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            ModelError::RetryDeadlineExceeded {
                attempts: 1,
                last_error: None,
                ..
            }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn does_not_replay_stream_after_visible_output() {
        let calls = Arc::new(AtomicU32::new(0));
        let model = {
            let calls = calls.clone();
            FunctionModel::with_stream(move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(stream::iter(vec![
                    Ok(serdes_ai_core::ModelResponseStreamEvent::part_start(
                        0,
                        ModelResponsePart::Text(TextPart::new("visible")),
                    )),
                    Err(ModelError::Connection("reset".into())),
                ]))
            })
        };
        let model = RetryingModel::new(
            model,
            RetryPolicy::for_model_requests()
                .max_attempts(3)
                .wait(serdes_ai_retries::WaitStrategy::None)
                .total_timeout(None),
        );
        let (messages, settings, params) = request_args();

        let events: Vec<_> = model
            .request_stream(&messages, &settings, &params)
            .await
            .unwrap()
            .collect()
            .await;

        assert_eq!(events.len(), 2);
        assert!(events[0].is_ok());
        assert!(matches!(events[1], Err(ModelError::Connection(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
