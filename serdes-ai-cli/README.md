# SerdesAI CLI

Command-line interface for SerdesAI - a feature-complete port of Code Puppy.

## Features

- Interactive chat mode with rich terminal UI
- Single-prompt execution mode
- Structured message rendering (V2 renderer)
- Command system with tab completion
- Session autosave and restore
- File attachment support
- Configuration via TOML

## Installation

Install from crates.io:

```bash
cargo install serdes-ai-cli
```

Or install from source:

```bash
git clone https://github.com/janfeddersen-wq/serdesAI
cd serdesAI
cargo install --path serdes-ai-cli
```

## Usage

```bash
# Interactive mode (default)
serdes-ai

# Single prompt
serdes-ai -p "Hello, world!"

# Specify model
serdes-ai -m gpt-4o

# Specify agent
serdes-ai -a code-puppy
```

## Commands

- `/help`, `/h` - Show help
- `/exit`, `/quit` - Exit
- `/cd [dir]` - Change directory
- `/clear` - Clear conversation
- `/model [name]` - Show/set model
- `/agent [name]` - Show/set agent
- `/session` - Show session info

## Configuration

- Config file: `~/.config/serdes-ai/config.toml`
- Supports TOML-based configuration for defaults and UI settings
- API keys should be provided via environment variables (provider-specific)

### Configuration Example

```toml
[general]
model = "gpt-4o"
agent = "code-puppy"

[colors]
banner_thinking = "bright_cyan"
banner_shell_command = "bright_green"
```

### Environment Variables

Set API keys in your shell profile or environment before running the CLI. Example:

```bash
export OPENAI_API_KEY="your-api-key"
```

Use the variable names required by the model/provider you configure.

## Key Bindings

- `Ctrl+C` - Cancel current operation
- `Ctrl+D` - Exit gracefully
- `Tab` - Command/path completion (planned)

## Development Status

- Core architecture: ✅ Complete
- V2 Renderer: ✅ Core implemented
- Command system: ✅ Basic commands
- Input system: 🚧 In progress
- MCP integration: 🚧 Planned
