//! MOTD (Message of the Day) command

use crate::bus;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::config;
use crate::register_command;

const DEFAULT_MOTD: &str = r#"
╔══════════════════════════════════════════════════════════════╗
║   Welcome to SerdesAI CLI!                                   ║
║                                                              ║
║  Type /help for available commands                           ║
║  Type @filename to attach files to your prompt              ║
║  Press Tab for command completion                            ║
╚══════════════════════════════════════════════════════════════╝
"#;

pub fn init() {
    register_command!(
        name = "motd",
        description = "Show the message of the day",
        usage = "/motd",
        aliases = [],
        category = CommandCategory::Core,
        handler = handle_motd
    )
    .ok();
}

fn handle_motd(_cmd: &str) -> CommandResult {
    // In real implementation, fetch from remote or cache
    let motd = DEFAULT_MOTD;
    bus::emit_info(motd.to_string());
    CommandResult::Handled
}

pub fn maybe_print_motd() {
    let marker = config::get_cache_dir().join("motd_last_shown");
    let should_show = should_show_motd(&marker);

    if should_show {
        bus::emit_info(DEFAULT_MOTD.to_string());
        let _ = std::fs::write(&marker, "");
    }
}

fn should_show_motd(marker: &std::path::Path) -> bool {
    use std::time::{Duration, SystemTime};

    match std::fs::metadata(marker) {
        Ok(meta) => {
            if let Ok(modified) = meta.modified() {
                if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                    return elapsed > Duration::from_secs(86400); // 24 hours
                }
            }
        }
        Err(_) => return true,
    }
    true
}
