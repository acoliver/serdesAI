//! Retry executor for running operations with retries.

use crate::config::{RetryConfig, RetryPolicy};
use crate::error::{RetryFailure, RetryResult, RetryableError};
use std::future::Future;
use std::time::Duration;
use tokio::time::{Instant, sleep, sleep_until, timeout_at};
use tracing::{debug, warn};

/// Classification returned to the generic retry executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Retry the operation, optionally honoring a provider-supplied delay.
    Retry {
        /// Provider-supplied minimum delay.
        retry_after: Option<Duration>,
    },
    /// Return the original error without another attempt.
    DoNotRetry,
}

/// Execute an operation under a provider-neutral retry policy.
///
/// The original error type is retained. Dropping this future cancels an
/// in-flight attempt or backoff because no background task is spawned.
pub async fn with_retry_policy<F, Fut, T, E, C>(
    policy: &RetryPolicy,
    mut operation: F,
    classify: C,
) -> Result<T, RetryFailure<E>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    C: Fn(&E) -> RetryDecision,
{
    let started = Instant::now();
    let deadline = policy.total_time_budget().map(|budget| started + budget);
    let mut attempts = 0;
    let mut last_error = None;

    loop {
        attempts += 1;
        let result = if let Some(deadline) = deadline {
            match timeout_at(deadline, operation()).await {
                Ok(result) => result,
                Err(_) => {
                    return Err(RetryFailure::DeadlineExceeded {
                        last_error,
                        attempts,
                        elapsed: started.elapsed(),
                    });
                }
            }
        } else {
            operation().await
        };

        match result {
            Ok(value) => return Ok(value),
            Err(error) => match classify(&error) {
                RetryDecision::DoNotRetry => {
                    return Err(RetryFailure::Permanent {
                        error,
                        attempts,
                        elapsed: started.elapsed(),
                    });
                }
                RetryDecision::Retry { retry_after } => {
                    if attempts >= policy.maximum_attempts() {
                        return Err(RetryFailure::Exhausted {
                            error,
                            attempts,
                            elapsed: started.elapsed(),
                        });
                    }

                    let wait = policy.wait_strategy().calculate(attempts, retry_after);
                    if let Some(deadline) = deadline {
                        let now = Instant::now();
                        if now >= deadline {
                            return Err(RetryFailure::DeadlineExceeded {
                                last_error: Some(error),
                                attempts,
                                elapsed: started.elapsed(),
                            });
                        }

                        let wake_at = now.checked_add(wait).unwrap_or(deadline);
                        if wake_at >= deadline {
                            last_error = Some(error);
                            sleep_until(deadline).await;
                            return Err(RetryFailure::DeadlineExceeded {
                                last_error,
                                attempts,
                                elapsed: started.elapsed(),
                            });
                        }
                    }

                    last_error = Some(error);
                    sleep(wait).await;
                }
            },
        }
    }
}

/// State of a retry attempt.
#[derive(Debug, Clone)]
pub struct RetryState {
    /// Current attempt number (1-indexed).
    pub attempt: u32,
    /// Last error message.
    pub last_error: Option<String>,
    /// Total time spent waiting.
    pub total_wait_time: Duration,
    /// History of attempts.
    pub history: Vec<AttemptInfo>,
}

impl Default for RetryState {
    fn default() -> Self {
        Self {
            attempt: 0,
            last_error: None,
            total_wait_time: Duration::ZERO,
            history: Vec::new(),
        }
    }
}

/// Information about a single attempt.
#[derive(Debug, Clone)]
pub struct AttemptInfo {
    /// Attempt number.
    pub attempt: u32,
    /// Whether it succeeded.
    pub success: bool,
    /// Error message if failed.
    pub error: Option<String>,
    /// Time waited before this attempt.
    pub wait_time: Duration,
}

/// Execute an operation with retries.
///
/// # Example
///
/// ```ignore
/// use serdes_ai_retries::{with_retry, RetryConfig};
///
/// let config = RetryConfig::for_api();
/// let result = with_retry(&config, || async {
///     // Your async operation here
///     Ok("success")
/// }).await?;
/// ```
pub async fn with_retry<F, Fut, T>(config: &RetryConfig, operation: F) -> RetryResult<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = RetryResult<T>>,
{
    let mut state = RetryState::default();
    let max_attempts = config.max_retries.saturating_add(1);

    loop {
        state.attempt += 1;

        debug!(
            attempt = state.attempt,
            max_attempts,
            max_retries = config.max_retries,
            "Executing retry attempt"
        );

        match operation().await {
            Ok(result) => {
                state.history.push(AttemptInfo {
                    attempt: state.attempt,
                    success: true,
                    error: None,
                    wait_time: Duration::ZERO,
                });
                return Ok(result);
            }
            Err(error) => {
                let should_retry =
                    state.attempt < max_attempts && config.retry_on.should_retry(&error);

                if !should_retry {
                    warn!(
                        attempt = state.attempt,
                        error = %error,
                        "Retry exhausted or error not retryable"
                    );
                    return Err(error);
                }

                let wait = config.wait.calculate(state.attempt, error.retry_after());
                state.total_wait_time += wait;
                state.last_error = Some(format!("{}", error));

                state.history.push(AttemptInfo {
                    attempt: state.attempt,
                    success: false,
                    error: Some(format!("{}", error)),
                    wait_time: wait,
                });

                debug!(
                    attempt = state.attempt,
                    wait_ms = wait.as_millis(),
                    error = %error,
                    "Waiting before retry"
                );

                sleep(wait).await;
            }
        }
    }
}

/// Execute with retries and get state information.
pub async fn with_retry_state<F, Fut, T>(
    config: &RetryConfig,
    operation: F,
) -> (RetryResult<T>, RetryState)
where
    F: Fn() -> Fut,
    Fut: Future<Output = RetryResult<T>>,
{
    let mut state = RetryState::default();
    let max_attempts = config.max_retries.saturating_add(1);

    loop {
        state.attempt += 1;

        match operation().await {
            Ok(result) => {
                state.history.push(AttemptInfo {
                    attempt: state.attempt,
                    success: true,
                    error: None,
                    wait_time: Duration::ZERO,
                });
                return (Ok(result), state);
            }
            Err(error) => {
                let should_retry =
                    state.attempt < max_attempts && config.retry_on.should_retry(&error);

                if !should_retry {
                    return (Err(error), state);
                }

                let wait = config.wait.calculate(state.attempt, error.retry_after());
                state.total_wait_time += wait;
                state.last_error = Some(format!("{}", error));

                state.history.push(AttemptInfo {
                    attempt: state.attempt,
                    success: false,
                    error: Some(format!("{}", error)),
                    wait_time: wait,
                });

                sleep(wait).await;
            }
        }
    }
}

/// Builder for retry operations.
pub struct Retry<'a> {
    config: &'a RetryConfig,
}

impl<'a> Retry<'a> {
    /// Create a new retry builder.
    pub fn new(config: &'a RetryConfig) -> Self {
        Self { config }
    }

    /// Run the operation with retries.
    pub async fn run<F, Fut, T>(self, operation: F) -> RetryResult<T>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = RetryResult<T>>,
    {
        with_retry(self.config, operation).await
    }

    /// Run and get state.
    pub async fn run_with_state<F, Fut, T>(self, operation: F) -> (RetryResult<T>, RetryState)
    where
        F: Fn() -> Fut,
        Fut: Future<Output = RetryResult<T>>,
    {
        with_retry_state(self.config, operation).await
    }
}

/// Wrap a result type for retry compatibility.
pub trait IntoRetryable<T> {
    /// Convert into a retryable result.
    fn into_retryable(self) -> RetryResult<T>;
}

impl<T, E: Into<RetryableError>> IntoRetryable<T> for Result<T, E> {
    fn into_retryable(self) -> RetryResult<T> {
        self.map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn test_with_retry_immediate_success() {
        let config = RetryConfig::new().max_retries(3);
        let result = with_retry(&config, || async { Ok::<_, RetryableError>(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_with_retry_eventual_success() {
        let config = RetryConfig::new()
            .max_retries(3)
            .fixed(Duration::from_millis(1));

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = attempts.clone();

        let result = with_retry(&config, || {
            let attempts = attempts_clone.clone();
            async move {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(RetryableError::http(500, "server error"))
                } else {
                    Ok(42)
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_with_retry_exhausted() {
        let config = RetryConfig::new()
            .max_retries(2)
            .fixed(Duration::from_millis(1));

        let result = with_retry(&config, || async {
            Err::<i32, _>(RetryableError::http(500, "always fails"))
        })
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_with_retry_non_retryable() {
        let config = RetryConfig::new().max_retries(3);

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = attempts.clone();

        let result = with_retry(&config, || {
            let attempts = attempts_clone.clone();
            async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<i32, _>(RetryableError::http(400, "bad request"))
            }
        })
        .await;

        assert!(result.is_err());
        // Should only try once since 400 is not retryable
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn generic_policy_honors_retry_after() {
        let policy = RetryPolicy::for_model_requests()
            .max_attempts(2)
            .wait(crate::WaitStrategy::RetryAfter {
                fallback: Box::new(crate::WaitStrategy::None),
                max_wait: Duration::from_secs(60),
            })
            .total_timeout(Some(Duration::from_secs(30)));
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();

        let task = tokio::spawn(async move {
            with_retry_policy(
                &policy,
                || {
                    let counter = counter.clone();
                    async move {
                        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                            Err::<u32, &'static str>("rate limited")
                        } else {
                            Ok(42)
                        }
                    }
                },
                |_| RetryDecision::Retry {
                    retry_after: Some(Duration::from_secs(5)),
                },
            )
            .await
        });

        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(4)).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        assert_eq!(task.await.unwrap().unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn total_deadline_prevents_another_attempt() {
        let policy = RetryPolicy::for_model_requests()
            .max_attempts(3)
            .wait(crate::WaitStrategy::Fixed(Duration::from_secs(10)))
            .total_timeout(Some(Duration::from_secs(3)));
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();

        let task = tokio::spawn(async move {
            with_retry_policy(
                &policy,
                || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Err::<(), &'static str>("transient")
                    }
                },
                |_| RetryDecision::Retry { retry_after: None },
            )
            .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(matches!(
            task.await.unwrap(),
            Err(RetryFailure::DeadlineExceeded { attempts: 1, .. })
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_future_cancels_backoff() {
        let policy = RetryPolicy::for_model_requests()
            .max_attempts(3)
            .wait(crate::WaitStrategy::Fixed(Duration::from_secs(10)))
            .total_timeout(None);
        let attempts = Arc::new(AtomicU32::new(0));
        let counter = attempts.clone();

        let task = tokio::spawn(async move {
            with_retry_policy(
                &policy,
                || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        Err::<(), &'static str>("transient")
                    }
                },
                |_| RetryDecision::Retry { retry_after: None },
            )
            .await
        });

        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        task.abort();
        tokio::time::advance(Duration::from_secs(20)).await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_state() {
        let config = RetryConfig::new()
            .max_retries(3)
            .fixed(Duration::from_millis(1));

        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = attempts.clone();

        let (result, state) = with_retry_state(&config, || {
            let attempts = attempts_clone.clone();
            async move {
                let n = attempts.fetch_add(1, Ordering::SeqCst);
                if n < 1 {
                    Err(RetryableError::http(500, "error"))
                } else {
                    Ok(42)
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(state.attempt, 2);
        assert_eq!(state.history.len(), 2);
        assert!(!state.history[0].success);
        assert!(state.history[1].success);
    }

    #[tokio::test]
    async fn test_retry_builder() {
        let config = RetryConfig::new();
        let result = Retry::new(&config)
            .run(|| async { Ok::<_, RetryableError>("success") })
            .await;

        assert_eq!(result.unwrap(), "success");
    }
}
