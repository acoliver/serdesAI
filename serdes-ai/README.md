# serdes-ai

[![Crates.io](https://img.shields.io/crates/v/serdes-ai.svg)](https://crates.io/crates/serdes-ai)
[![Documentation](https://docs.rs/serdes-ai/badge.svg)](https://docs.rs/serdes-ai)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/janfeddersen-wq/serdesAI/blob/main/LICENSE)

> Type-safe, production-ready AI agent framework for Rust - a full port of pydantic-ai

This is the main facade crate that re-exports all SerdesAI functionality for convenient use.

## Installation

```toml
[dependencies]
serdes-ai = "0.1"
```

## Quick Start

```rust
use serdes_ai::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let agent = Agent::new(OpenAIChatModel::from_env("gpt-4o")?)
        .system_prompt("You are a helpful assistant.")
        .build();
    
    let result = agent.run("Hello!", ()).await?;
    println!("{}", result.output);
    
    Ok(())
}
```

## Features

- 🤖 **Type-safe Agents** - Generic over dependencies and output types
- 🔌 **Multi-provider Support** - OpenAI, Anthropic, Google, Groq, Mistral, Ollama, and more
- 🛠️ **Tool Calling** - Define tools with automatic JSON schema generation
- 📡 **Streaming** - Real-time response streaming
- 🔄 **Smart Retries** - Configurable retry strategies
- 🔀 **Graph Workflows** - Complex multi-agent orchestration

## Configuring retries

```rust,ignore
use serdes_ai::prelude::*;
use std::time::Duration;

let model = OpenAIChatModel::from_env("gpt-4o")?.with_retries(
    RetryPolicy::for_model_requests()
        .max_attempts(3)
        .total_timeout(Some(Duration::from_secs(30))),
);
let agent = AgentBuilder::new(model).build();
```

Retries are opt-in and repeat the same model. `FallbackModel` remains the
separate mechanism for selecting another model. Streaming acquisition can retry
before its first visible event; errors after that boundary are never replayed.

## Part of SerdesAI

This crate is part of the [SerdesAI](https://github.com/janfeddersen-wq/serdesAI) workspace.

## License

MIT License - see [LICENSE](https://github.com/janfeddersen-wq/serdesAI/blob/main/LICENSE) for details.
