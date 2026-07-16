# serdes-ai

[![Crates.io](https://img.shields.io/crates/v/serdes-ai.svg)](https://crates.io/crates/serdes-ai)
[![Documentation](https://docs.rs/serdes-ai/badge.svg)](https://docs.rs/serdes-ai)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/janfeddersen-wq/serdesAI/blob/main/LICENSE)

> Type-safe Rust AI agent framework inspired by pydantic-ai; parity is ongoing

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
separate mechanism for selecting another model. A streaming retry or fallback
can occur only before its first event; metadata also counts as exposure. Errors
after that boundary are propagated without replay or concatenation.

## Parity and readiness

SerdesAI provides Rust-native typed agents, tools, streaming, retries, fallback,
graphs, MCP, embeddings, and evaluation crates. It does not currently claim full
API or behavioral parity with pydantic-ai. The capability matrix uses pydantic-ai
v2.9.1 (`bf9a2435de41aaf269fc6bc72fe641f2fa0465c6`) as its comparison target and
SerdesAI upstream revision `be5774b5c618a71fe899ac5b8c6a5e958ea42a5d` as the
implementation audit baseline. Consult the root README matrix and test the exact
providers and features required by your deployment before treating a
configuration as production-ready.

## Part of SerdesAI

This crate is part of the [SerdesAI](https://github.com/janfeddersen-wq/serdesAI) workspace.

## License

MIT License - see [LICENSE](https://github.com/janfeddersen-wq/serdesAI/blob/main/LICENSE) for details.
