//! Session commands - /session, /autosave_load, /history

use crate::bus;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::config;
use crate::register_command;
use crate::session;
use crate::tui::interactive_autosave_menu;

pub fn init() {
    register_command!(
        name = "compact",
        description = "Compact current session history",
        usage = "/compact",
        aliases = [],
        category = CommandCategory::Session,
        handler = handle_compact
    )
    .ok();

    register_command!(
        name = "truncate",
        description = "Keep only N most recent session messages",
        usage = "/truncate <N>",
        aliases = [],
        category = CommandCategory::Session,
        handler = handle_truncate
    )
    .ok();

    register_command!(
        name = "dump_context",
        description = "Dump current session context to a named JSON file",
        usage = "/dump_context <name>",
        aliases = [],
        category = CommandCategory::Session,
        handler = handle_dump_context
    )
    .ok();

    register_command!(
        name = "load_context",
        description = "Load a named context JSON into the current session",
        usage = "/load_context <name>",
        aliases = [],
        category = CommandCategory::Session,
        handler = handle_load_context
    )
    .ok();

    register_command!(
        name = "autosave_load",
        description = "Load an autosaved session interactively",
        usage = "/autosave_load",
        aliases = ["resume"],
        category = CommandCategory::Session,
        handler = handle_autosave_load
    )
    .ok();

    register_command!(
        name = "history",
        description = "Show command history",
        usage = "/history [count]",
        aliases = [],
        category = CommandCategory::Session,
        handler = handle_history
    )
    .ok();
}

fn handle_compact(_cmd: &str) -> CommandResult {
    let autosave_dir = config::get_autosave_dir();
    let mut current = match load_current_session_snapshot(&autosave_dir) {
        Ok(s) => s,
        Err(err) => {
            bus::emit_error(format!("Failed to load current session: {err}"));
            return CommandResult::Handled;
        }
    };

    let strategy = config::get_compaction_strategy();
    let protected_tokens = config::get_protected_token_count();

    let compacted = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(session::compact_history(
            &mut current.messages,
            strategy.clone(),
            protected_tokens,
        ))
    });

    match compacted {
        Ok(stats) => {
            current.updated_at = chrono::Utc::now();
            if let Err(err) = session::autosave_current_session(&current) {
                bus::emit_error(format!(
                    "Compacted in memory, but failed to persist session: {err}"
                ));
                return CommandResult::Handled;
            }

            let token_reduction = stats.before_tokens.saturating_sub(stats.after_tokens);
            let reduction_pct = if stats.before_tokens == 0 {
                0.0
            } else {
                (token_reduction as f64 / stats.before_tokens as f64) * 100.0
            };

            bus::emit_info(format!(
                "Compaction stats: messages {} -> {}, tokens {} -> {}",
                stats.before_count, stats.after_count, stats.before_tokens, stats.after_tokens
            ));
            bus::emit_success(format!(
                "Compacted using {:?}. Token reduction: {} ({:.1}%)",
                stats.strategy, token_reduction, reduction_pct
            ));
        }
        Err(err) => bus::emit_error(format!("Failed to compact history: {err}")),
    }

    CommandResult::Handled
}

fn handle_truncate(cmd: &str) -> CommandResult {
    let n_raw = cmd.split_whitespace().nth(1);
    let Some(n_raw) = n_raw else {
        bus::emit_error("Usage: /truncate <N>".to_string());
        return CommandResult::Handled;
    };

    let n = match n_raw.parse::<usize>() {
        Ok(v) if v >= 1 => v,
        _ => {
            bus::emit_error(format!("Invalid N '{}'. Expected integer >= 1.", n_raw));
            return CommandResult::Handled;
        }
    };

    let autosave_dir = config::get_autosave_dir();
    let mut current = match load_current_session_snapshot(&autosave_dir) {
        Ok(s) => s,
        Err(err) => {
            bus::emit_error(format!("Failed to load current session: {err}"));
            return CommandResult::Handled;
        }
    };

    let before = current.messages.len();
    match session::truncate_history(&mut current.messages, n) {
        Ok(_) => {
            current.updated_at = chrono::Utc::now();
            if let Err(err) = session::autosave_current_session(&current) {
                bus::emit_error(format!(
                    "Truncated in memory, but failed to persist session: {err}"
                ));
                return CommandResult::Handled;
            }

            let after = current.messages.len();
            bus::emit_success(format!("Truncated from {} to {} messages", before, after));
        }
        Err(err) => bus::emit_error(format!("Failed to truncate history: {err}")),
    }

    CommandResult::Handled
}

fn handle_dump_context(cmd: &str) -> CommandResult {
    let name = cmd.split_whitespace().nth(1);
    let Some(name) = name else {
        bus::emit_error("Usage: /dump_context <name>".to_string());
        return CommandResult::Handled;
    };

    let autosave_dir = config::get_autosave_dir();
    let current = match load_current_session_snapshot(&autosave_dir) {
        Ok(s) => s,
        Err(err) => {
            bus::emit_error(format!("Failed to load current session: {err}"));
            return CommandResult::Handled;
        }
    };

    match session::dump_context(name, &current.messages) {
        Ok(path) => bus::emit_success(format!("Context '{}' dumped to {}", name, path.display())),
        Err(err) => bus::emit_error(format!("Failed to dump context: {err}")),
    }

    CommandResult::Handled
}

fn handle_load_context(cmd: &str) -> CommandResult {
    let name = cmd.split_whitespace().nth(1);
    let Some(name) = name else {
        bus::emit_error("Usage: /load_context <name>".to_string());
        return CommandResult::Handled;
    };

    match session::load_context(name) {
        Ok(messages) => {
            let session_id = session::rotate_session_now();
            let snapshot = session::Session {
                id: session_id.clone(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                messages,
            };

            if let Err(err) = session::autosave_current_session(&snapshot) {
                bus::emit_error(format!(
                    "Context loaded but failed to persist session: {err}"
                ));
                return CommandResult::Handled;
            }

            bus::emit_success(format!(
                "Loaded context '{}' into new session {} ({} messages)",
                name,
                session_id,
                snapshot.messages.len()
            ));
            session::display_resumed_history(
                &snapshot.messages,
                config::get_resume_message_count(),
            );
        }
        Err(err) => bus::emit_error(format!("Failed to load context: {err}")),
    }

    CommandResult::Handled
}

fn handle_autosave_load(_cmd: &str) -> CommandResult {
    match interactive_autosave_menu() {
        Ok(Some(session_id)) => {
            let autosave_dir = config::get_autosave_dir();
            match session::load_session(&session_id, &autosave_dir) {
                Ok(session_data) => {
                    session::set_current_session_id(session_id.clone());
                    bus::emit_success(format!(
                        "Loaded session: {} ({} messages)",
                        session_id,
                        session_data.messages.len()
                    ));
                    session::display_resumed_history(
                        &session_data.messages,
                        config::get_resume_message_count(),
                    );
                }
                Err(e) => {
                    bus::emit_error(format!("Failed to load session: {}", e));
                }
            }
        }
        Ok(None) => {
            bus::emit_info("Session selection cancelled");
        }
        Err(e) => {
            bus::emit_error(format!("Autosave menu failed: {}", e));
        }
    }
    CommandResult::Handled
}

fn load_current_session_snapshot(
    autosave_dir: &std::path::Path,
) -> anyhow::Result<session::Session> {
    let session_id = session::get_current_session_id()
        .ok_or_else(|| anyhow::anyhow!("no active session id; start a conversation first"))?;
    session::load_session(&session_id, autosave_dir)
}

fn handle_history(cmd: &str) -> CommandResult {
    let arg = cmd.split_whitespace().nth(1).unwrap_or("20");
    let count: usize = arg.parse().unwrap_or(20);

    let history_file = config::get_command_history_file();

    match std::fs::read_to_string(&history_file) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(count);

            bus::emit_info(format!("Last {} commands:", lines.len() - start));
            for (i, line) in lines.iter().enumerate().skip(start) {
                bus::emit_info(format!("  {}: {}", i + 1, line));
            }
        }
        Err(err) => {
            bus::emit_warning(format!("No command history available: {}", err));
        }
    }

    CommandResult::Handled
}
