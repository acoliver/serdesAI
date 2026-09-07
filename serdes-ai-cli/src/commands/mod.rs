//! Command implementations

pub mod api;
pub mod autosave;
pub mod config;
pub mod config_cmds;
pub mod core;
pub mod mcp;
pub mod model;
pub mod motd;
pub mod paste;
pub mod registry;
pub mod session;
pub mod special;
pub mod tools;
pub mod uc;

use crate::bus::MessageBus;
use crate::commands::registry::{CommandResult, execute_command, get_all_commands};

/// Initialize all built-in command modules.
pub fn init_all() {
    registry::clear_registry(); // Clear any existing registrations

    // Initialize each command module
    core::init();
    session::init();
    config_cmds::init();
    special::init();
    tokio::spawn(async {
        mcp::init().await;
    });
    uc::init();
    autosave::init();
    model::init();
    api::init();
}

/// Back-compat alias for older call sites.
pub fn init() {
    init_all();
}

/// Main command handler
pub fn handle_command(bus: &MessageBus, command: &str) -> CommandResult {
    let _ = bus;

    if get_all_commands().is_empty() {
        init_all();
    }

    execute_command(command)
}
