//! Tools command - displays available tools

use crate::bus;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::register_command;

const TOOLS_CONTENT: &str = r#"
# Available Tools

## File Operations
- **read_file** - Read file contents with line range support
- **list_files** - List directory contents recursively
- **grep** - Search files with regex patterns

## Shell
- **run_shell** - Execute shell commands safely
- **run_shell_interactive** - Interactive shell sessions

## Code
- **apply_diff** - Apply code changes from diffs
- **read_file_tool** - Enhanced file reading with context

## Web
- **fetch_url** - Fetch content from URLs

## MCP
- **list_mcp_servers** - List configured MCP servers
- **run_mcp_tool** - Execute MCP server tools

Type /help for more commands.
"#;

pub fn init() {
    register_command!(
        name = "tools",
        description = "Show available tools and capabilities",
        usage = "/tools",
        aliases = [],
        category = CommandCategory::Tools,
        handler = handle_tools
    )
    .ok();
}

fn handle_tools(_cmd: &str) -> CommandResult {
    bus::emit_info(TOOLS_CONTENT.to_string());
    CommandResult::Handled
}
