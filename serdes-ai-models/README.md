# serdes-ai-models

[![Crates.io](https://img.shields.io/crates/v/serdes-ai-models.svg)](https://crates.io/crates/serdes-ai-models)
[![Documentation](https://docs.rs/serdes-ai-models/badge.svg)](https://docs.rs/serdes-ai-models)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/janfeddersen-wq/serdesAI/blob/main/LICENSE)

> Model trait and provider implementations for serdes-ai

This crate defines the `Model` trait and provides implementations for various LLM providers:

- OpenAI (GPT-4, GPT-4o, o1, o3)
- Anthropic (Claude 3.5, Claude 4)
- Google (Gemini 1.5, Gemini 2.0)
- Groq (Llama, Mixtral)
- Mistral
- Ollama (local models)
- Azure OpenAI
- AWS Bedrock

## Installation

```toml
[dependencies]
serdes-ai-models = "0.1"
```

## Usage

```rust,ignore
use serdes_ai_models::{Model, ModelRetryExt, OpenAIChatModel, RetryPolicy};
use std::time::Duration;

let model = OpenAIChatModel::from_env("gpt-4o")?.with_retries(
    RetryPolicy::for_model_requests()
        .max_attempts(3)
        .total_timeout(Some(Duration::from_secs(30))),
);

let response = model.request(&messages, &settings, &params).await?;
```

Retries are opt-in. `RetryPolicy::disabled()` performs exactly one attempt. The
policy retries the same model; `FallbackModel` remains responsible for selecting
a different model. Streaming requests may be retried only while acquiring the
stream and before the first caller-visible event. Once an event is returned, later
stream errors pass through without replaying or concatenating another response.

## Part of SerdesAI

This crate is part of the [SerdesAI](https://github.com/janfeddersen-wq/serdesAI) workspace.

For most use cases, you should use the main `serdes-ai` crate which re-exports these types.

## License

MIT License - see [LICENSE](https://github.com/janfeddersen-wq/serdesAI/blob/main/LICENSE) for details.
