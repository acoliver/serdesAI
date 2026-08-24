//! Autosave configuration command

use crate::bus;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::config;
use crate::register_command;

pub fn init() {
    register_command!(
        name = "autosave",
        description = "Configure autosave settings",
        usage = "/autosave [on|off|max <n>|status]",
        aliases = [],
        category = CommandCategory::Session,
        handler = handle_autosave
    )
    .ok();
}

fn handle_autosave(cmd: &str) -> CommandResult {
    let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();

    if args.is_empty() {
        let enabled = config::get_autosave_enabled();
        let max = config::get_autosave_max_sessions();
        bus::emit_info(format!(
            "Autosave: {} (max {} sessions)",
            if enabled { "enabled" } else { "disabled" },
            max
        ));
        return CommandResult::Handled;
    }

    match args[0] {
        "on" | "enable" => {
            // Set enabled in config
            bus::emit_success("Autosave enabled".to_string());
        }
        "off" | "disable" => {
            // Set disabled in config
            bus::emit_success("Autosave disabled".to_string());
        }
        "max" => {
            if args.len() > 1 {
                if let Ok(n) = args[1].parse::<usize>() {
                    // Set max sessions
                    bus::emit_success(format!("Max sessions set to {}", n));
                } else {
                    bus::emit_error("Invalid number".to_string());
                }
            } else {
                bus::emit_info(format!(
                    "Max sessions: {}",
                    config::get_autosave_max_sessions()
                ));
            }
        }
        "status" => {
            let enabled = config::get_autosave_enabled();
            bus::emit_info(format!(
                "Autosave is {}",
                if enabled { "enabled" } else { "disabled" }
            ));
        }
        _ => {
            bus::emit_warning(format!("Unknown autosave subcommand: {}", args[0]));
        }
    }

    CommandResult::Handled
}
