//! MCP (Model Context Protocol) commands with real runtime

use crate::bus;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::mcp_runtime::{self, McpRuntime, ServerStatus};
use crate::register_command;
use std::sync::Arc;
use tokio::sync::Mutex;

static MCP_RUNTIME: once_cell::sync::Lazy<Arc<Mutex<McpRuntime>>> =
    once_cell::sync::Lazy::new(|| Arc::new(Mutex::new(McpRuntime::new())));

pub async fn init() {
    // Load MCP config on startup
    let mut runtime = MCP_RUNTIME.lock().await;
    if let Err(e) = runtime.load_config() {
        bus::emit_warning(format!("Failed to load MCP config: {}", e));
    }

    register_command!(
        name = "mcp",
        description = "MCP server management (list, install, remove, start, stop, status, logs)",
        usage = "/mcp <subcommand>",
        aliases = [],
        category = CommandCategory::Mcp,
        handler = handle_mcp
    )
    .ok();
}

fn handle_mcp(cmd: &str) -> CommandResult {
    // Spawn async handler
    let cmd = cmd.to_string();
    tokio::spawn(async move {
        if let Err(e) = handle_mcp_async(&cmd).await {
            bus::emit_error(format!("MCP command failed: {}", e));
        }
    });

    CommandResult::Handled
}

async fn handle_mcp_async(cmd: &str) -> anyhow::Result<()> {
    let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();

    if args.is_empty() {
        show_mcp_help();
        return Ok(());
    }

    match args[0] {
        "list" => {
            let runtime = MCP_RUNTIME.lock().await;
            let servers = runtime.list_servers();

            if servers.is_empty() {
                bus::emit_info("No MCP servers configured.".to_string());
                bus::emit_info(
                    "Use '/mcp install <name> <command> [args...]' to add one.".to_string(),
                );
            } else {
                bus::emit_info("Configured MCP servers:".to_string());
                for (name, status) in servers {
                    let status_str = match status {
                        ServerStatus::Running => "🟢 running",
                        ServerStatus::Starting => "🟡 starting",
                        ServerStatus::Stopped => "⚪ stopped",
                        ServerStatus::Error(_) => "🔴 error",
                    };
                    bus::emit_info(format!("  {} - {}", name, status_str));
                }
            }
        }
        "install" => {
            if args.len() < 3 {
                bus::emit_error("Usage: /mcp install <name> <command> [args...]".to_string());
                return Ok(());
            }

            let name = args[1];
            let command = args[2];
            let cmd_args: Vec<String> = args.iter().skip(3).map(|s| s.to_string()).collect();

            match mcp_runtime::install_server(name, command, &cmd_args).await {
                Ok(()) => {
                    bus::emit_success(format!("MCP server '{}' installed successfully", name))
                }
                Err(e) => bus::emit_error(format!("Failed to install: {}", e)),
            }
        }
        "remove" => {
            if args.len() < 2 {
                bus::emit_error("Usage: /mcp remove <name>".to_string());
                return Ok(());
            }

            let name = args[1];
            match mcp_runtime::remove_server(name).await {
                Ok(()) => bus::emit_success(format!("MCP server '{}' removed", name)),
                Err(e) => bus::emit_error(format!("Failed to remove: {}", e)),
            }
        }
        "start" => {
            if args.len() < 2 {
                bus::emit_error("Usage: /mcp start <name>".to_string());
                return Ok(());
            }

            let name = args[1];
            let mut runtime = MCP_RUNTIME.lock().await;
            match runtime.start_server(name).await {
                Ok(()) => bus::emit_success(format!("MCP server '{}' started", name)),
                Err(e) => bus::emit_error(format!("Failed to start: {}", e)),
            }
        }
        "stop" => {
            if args.len() < 2 {
                bus::emit_error("Usage: /mcp stop <name>".to_string());
                return Ok(());
            }

            let name = args[1];
            let mut runtime = MCP_RUNTIME.lock().await;
            match runtime.stop_server(name).await {
                Ok(()) => bus::emit_success(format!("MCP server '{}' stopped", name)),
                Err(e) => bus::emit_error(format!("Failed to stop: {}", e)),
            }
        }
        "status" => {
            if args.len() < 2 {
                let runtime = MCP_RUNTIME.lock().await;
                let servers = runtime.list_servers();
                for (name, status) in servers {
                    let status_str = format!("{:?}", status);
                    bus::emit_info(format!("{}: {}", name, status_str));
                }
            } else {
                let name = args[1];
                let runtime = MCP_RUNTIME.lock().await;
                match runtime.get_status(name) {
                    Some(status) => bus::emit_info(format!("{}: {:?}", name, status)),
                    None => bus::emit_warning(format!("Server '{}' not found", name)),
                }
            }
        }
        "logs" => {
            bus::emit_info("MCP logs: (not yet implemented)".to_string());
        }
        _ => {
            bus::emit_warning(format!("Unknown MCP subcommand: {}", args[0]));
            show_mcp_help();
        }
    }

    Ok(())
}

fn show_mcp_help() {
    bus::emit_info("MCP commands:".to_string());
    bus::emit_info("  /mcp list              - List MCP servers".to_string());
    bus::emit_info(
        "  /mcp install <name> <command> [args...]  - Install an MCP server".to_string(),
    );
    bus::emit_info("  /mcp remove <name>     - Remove an MCP server".to_string());
    bus::emit_info("  /mcp start <name>      - Start MCP server".to_string());
    bus::emit_info("  /mcp stop <name>       - Stop MCP server".to_string());
    bus::emit_info("  /mcp status [name]     - Show MCP server status".to_string());
    bus::emit_info("  /mcp logs <name>       - Show MCP server logs".to_string());
}
