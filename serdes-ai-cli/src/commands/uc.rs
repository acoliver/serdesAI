//! UC commands: /uc <subcommand>

use crate::bus;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::register_command;
use crate::uc;

pub fn init() {
    register_command!(
        name = "uc",
        description = "Universal Constructor management",
        usage = "/uc <status|enable|disable|sandbox|create|list|remove>",
        aliases = [],
        category = CommandCategory::Uc,
        handler = handle_uc
    )
    .ok();
}

fn handle_uc(cmd: &str) -> CommandResult {
    let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();

    if args.is_empty() {
        return handle_uc_status();
    }

    match args[0] {
        "status" => handle_uc_status(),
        "enable" => handle_uc_enable(),
        "disable" => handle_uc_disable(),
        "sandbox" => handle_uc_sandbox(&args),
        "create" => handle_uc_create(&args),
        "list" => handle_uc_list(),
        "remove" => handle_uc_remove(&args),
        _ => {
            bus::emit_warning(format!("Unknown UC subcommand: {}", args[0]));
            show_uc_help();
            CommandResult::Handled
        }
    }
}

fn handle_uc_status() -> CommandResult {
    let enabled = uc::is_enabled();
    let sandbox = uc::is_sandbox_enabled();
    let tool_count = uc::list_tools().len();

    bus::emit_info("Universal Constructor Status:".to_string());
    bus::emit_info(format!("  Enabled: {}", if enabled { "yes" } else { "no" }));
    bus::emit_info(format!("  Sandbox: {}", if sandbox { "yes" } else { "no" }));
    bus::emit_info(format!("  Tools: {}", tool_count));

    if !enabled {
        bus::emit_info("\nUse /uc enable to activate".to_string());
    }

    CommandResult::Handled
}

fn handle_uc_enable() -> CommandResult {
    uc::set_enabled(true);
    bus::emit_success("Universal Constructor enabled".to_string());
    CommandResult::Handled
}

fn handle_uc_disable() -> CommandResult {
    uc::set_enabled(false);
    bus::emit_success("Universal Constructor disabled".to_string());
    CommandResult::Handled
}

fn handle_uc_sandbox(args: &[&str]) -> CommandResult {
    if args.len() < 2 {
        let enabled = uc::is_sandbox_enabled();
        bus::emit_info(format!(
            "Sandbox mode: {}",
            if enabled { "enabled" } else { "disabled" }
        ));
        return CommandResult::Handled;
    }

    match args[1] {
        "on" | "enable" => {
            uc::set_sandbox_enabled(true);
            bus::emit_success("Sandbox mode enabled".to_string());
        }
        "off" | "disable" => {
            uc::set_sandbox_enabled(false);
            bus::emit_warning("Sandbox mode disabled - tools run without isolation".to_string());
        }
        _ => {
            bus::emit_error("Usage: /uc sandbox <on|off>".to_string());
        }
    }
    CommandResult::Handled
}

fn handle_uc_create(_args: &[&str]) -> CommandResult {
    // In a real implementation, this would open an editor or prompt
    // For now, emit instructions
    bus::emit_info("To create a UC tool:".to_string());
    bus::emit_info("1. The agent can create tools dynamically".to_string());
    bus::emit_info("2. Ask the agent to create a tool for you".to_string());
    bus::emit_info("3. Or use the UC API directly".to_string());
    CommandResult::Handled
}

fn handle_uc_list() -> CommandResult {
    let tools = uc::list_tools();
    if tools.is_empty() {
        bus::emit_info("No UC tools created yet".to_string());
    } else {
        bus::emit_info(format!("{} UC tools:", tools.len()));
        for tool in tools {
            bus::emit_info(format!("  {} - {}", tool.name, tool.description));
        }
    }
    CommandResult::Handled
}

fn handle_uc_remove(args: &[&str]) -> CommandResult {
    if args.len() < 2 {
        bus::emit_error("Usage: /uc remove <tool-name>".to_string());
        return CommandResult::Handled;
    }

    let name = args[1];
    if uc::remove_tool(name) {
        bus::emit_success(format!("Removed UC tool: {}", name));
    } else {
        bus::emit_warning(format!("UC tool not found: {}", name));
    }
    CommandResult::Handled
}

fn show_uc_help() {
    bus::emit_info("UC commands:".to_string());
    bus::emit_info("  /uc status           - Show UC status".to_string());
    bus::emit_info("  /uc enable           - Enable UC".to_string());
    bus::emit_info("  /uc disable          - Disable UC".to_string());
    bus::emit_info("  /uc sandbox [on|off] - Toggle sandbox".to_string());
    bus::emit_info("  /uc create           - Create tool (via agent)".to_string());
    bus::emit_info("  /uc list             - List created tools".to_string());
    bus::emit_info("  /uc remove <name>    - Remove a tool".to_string());
}
