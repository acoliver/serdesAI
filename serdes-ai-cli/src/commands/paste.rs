//! Paste command - paste image from clipboard

use crate::bus;
use crate::clipboard;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::register_command;

pub fn init() {
    register_command!(
        name = "paste",
        description = "Paste image from clipboard",
        usage = "/paste",
        aliases = ["clipboard", "cb"],
        category = CommandCategory::Core,
        handler = handle_paste
    )
    .ok();
}

fn handle_paste(_cmd: &str) -> CommandResult {
    if !clipboard::has_image_in_clipboard() {
        bus::emit_warning("No image found in clipboard".to_string());
        bus::emit_info("Copy an image and try again".to_string());
        return CommandResult::Handled;
    }

    match clipboard::capture_clipboard_image_to_pending() {
        Some(placeholder) => {
            let count = clipboard::get_pending_count();
            bus::emit_success(format!("Captured: {}", placeholder));
            bus::emit_info(format!("Total pending images: {}", count));
        }
        None => {
            bus::emit_warning("Failed to capture clipboard image".to_string());
        }
    }

    CommandResult::Handled
}
