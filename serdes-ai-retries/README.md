# serdes-ai-retries

[![Crates.io](https://img.shields.io/crates/v/serdes-ai-retries.svg)](https://crates.io/crates/serdes-ai-retries)
[![Documentation](https://docs.rs/serdes-ai-retries/badge.svg)](https://docs.rs/serdes-ai-retries)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/janfeddersen-wq/serdesAI/blob/main/LICENSE)

> Retry strategies and error handling for serdes-ai

This crate provides retry capabilities for SerdesAI:

- Configurable retry strategies
- Exponential backoff with jitter
- Rate limit handling
- Transient error detection

## Installation

```toml
[dependencies]
serdes-ai-retries = "0.1"
```

## Usage

The provider-neutral executor retains the operation's original error type. Policies
count total attempts, include backoff in the total time budget, and honor
`Retry-After` when the classifier supplies it.

```rust,no_run
use serdes_ai_retries::{
    with_retry_policy, RetryDecision, RetryPolicy, WaitStrategy,
};
use std::time::Duration;

# async fn example() {
let policy = RetryPolicy::for_model_requests()
    .max_attempts(3)
    .wait(WaitStrategy::Fixed(Duration::from_millis(100)))
    .total_timeout(Some(Duration::from_secs(10)));

let result = with_retry_policy(
    &policy,
    || async { Ok::<_, std::io::Error>("success") },
    |error| match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::ConnectionReset => {
            RetryDecision::Retry { retry_after: None }
        }
        _ => RetryDecision::DoNotRetry,
    },
).await;
# assert_eq!(result.unwrap(), "success");
# }
```

Dropping the returned future cancels an in-flight attempt or backoff immediately;
the executor does not spawn a background retry task. Use
`RetryPolicy::disabled()` for an explicit one-attempt policy.

## Part of SerdesAI

This crate is part of the [SerdesAI](https://github.com/janfeddersen-wq/serdesAI) workspace.

For most use cases, you should use the main `serdes-ai` crate which re-exports these types.

## License

MIT License - see [LICENSE](https://github.com/janfeddersen-wq/serdesAI/blob/main/LICENSE) for details.
